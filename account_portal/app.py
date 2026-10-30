from __future__ import annotations

import hmac
import ipaddress
import json
import os
import re
import secrets
import threading
from dataclasses import dataclass, field
from functools import wraps
from pathlib import Path
from typing import Any, Callable, TypeVar
from urllib.parse import urlencode, urlsplit

import requests
from dotenv import load_dotenv
from flask import Flask, abort, flash, redirect, render_template, request, session, url_for


APP_DIR = Path(__file__).resolve().parent
load_dotenv(APP_DIR / ".env")

EMAIL_RE = re.compile(r"^[^\s@]+@[^\s@]+\.[^\s@]+$")
USERNAME_RE = re.compile(r"^[^\x00-\x1f\x7f]{1,128}$")
REQUEST_TIMEOUT_SECONDS = 10
MAX_RESPONSE_BYTES = 64 * 1024


@dataclass
class NakamaSession:
    access_token: str = field(repr=False)
    refresh_token: str = field(repr=False)
    email: str


F = TypeVar("F", bound=Callable[..., Any])


def create_app(test_config: dict[str, Any] | None = None) -> Flask:
    app = Flask(__name__)
    configured_secret = os.environ.get("FLASK_SECRET_KEY", "")
    app.config.update(
        SECRET_KEY=configured_secret or secrets.token_urlsafe(48),
        MAX_CONTENT_LENGTH=32 * 1024,
        SESSION_COOKIE_HTTPONLY=True,
        SESSION_COOKIE_SAMESITE="Lax",
        SESSION_COOKIE_SECURE=os.environ.get("COOKIE_SECURE", "false").lower()
        in {"1", "true", "yes"},
        PERMANENT_SESSION_LIFETIME=3600,
        NAKAMA_URL=os.environ.get("NAKAMA_URL", "http://127.0.0.1:7350"),
        NAKAMA_SERVER_KEY=os.environ.get("NAKAMA_SERVER_KEY", ""),
    )
    if test_config:
        app.config.update(test_config)

    # Tokens exist only in process memory. Flask's signed browser cookie stores
    # an opaque random key, never Nakama access/refresh tokens or passwords.
    app.extensions["nakama_sessions"] = {}
    app.extensions["nakama_sessions_lock"] = threading.RLock()

    @app.context_processor
    def csrf_context() -> dict[str, str]:
        return {"csrf_token": csrf_token()}

    @app.before_request
    def protect_post_requests() -> None:
        if request.method != "POST":
            return
        expected = session.get("csrf_token", "")
        supplied = request.form.get("csrf_token", "")
        if not expected or not hmac.compare_digest(expected, supplied):
            abort(400)

    @app.get("/")
    def index():
        current = current_session()
        if current:
            return render_template("account.html", account_email=current.email)
        return render_template("index.html")

    @app.route("/register", methods=["GET", "POST"])
    def register():
        if request.method == "GET":
            return render_template("register.html", email="", username="")

        email = request.form.get("email", "").strip()
        username = request.form.get("username", "").strip()
        password = request.form.get("password", "")
        if not EMAIL_RE.fullmatch(email) or len(email) > 255:
            flash("Enter a valid email address.", "error")
            return render_template("register.html", email=email, username=username), 400
        if username and (len(username) > 128 or not USERNAME_RE.fullmatch(username)):
            flash("Username must be at most 128 characters without control characters.", "error")
            return render_template("register.html", email=email, username=username), 400
        if not 8 <= len(password) <= 128:
            flash("Password must be between 8 and 128 characters.", "error")
            return render_template("register.html", email=email, username=username), 400

        try:
            tokens = authenticate_email(
                app,
                email=email,
                password=password,
                create=True,
                username=username or None,
            )
        except PortalError as error:
            flash(error.message, "error")
            return render_template("register.html", email=email, username=username), error.status

        establish_session(tokens, email)
        flash("Account created and signed in.", "success")
        return redirect(url_for("index"))

    @app.route("/login", methods=["GET", "POST"])
    def login():
        if request.method == "GET":
            return render_template("login.html", email="")

        email = request.form.get("email", "").strip()
        password = request.form.get("password", "")
        if not EMAIL_RE.fullmatch(email) or len(email) > 255 or not 1 <= len(password) <= 128:
            flash("Check your email and password, then try again.", "error")
            return render_template("login.html", email=email), 400

        try:
            tokens = authenticate_email(
                app, email=email, password=password, create=False, username=None
            )
        except PortalError as error:
            flash(error.message, "error")
            return render_template("login.html", email=email), error.status

        establish_session(tokens, email)
        flash("Signed in.", "success")
        return redirect(url_for("index"))

    @app.post("/logout")
    def logout():
        current = current_session()
        if current:
            try:
                revoke_session(app, current)
            except PortalError:
                # Always clear the local session. The page warns that upstream
                # revocation could not be confirmed; tokens are not shown/logged.
                flash("Signed out locally; Nakama revocation could not be confirmed.", "error")
            else:
                flash("Signed out.", "success")
            remove_session()
        session.clear()
        return redirect(url_for("index"))

    def login_required(view: F) -> F:
        @wraps(view)
        def wrapped(*args: Any, **kwargs: Any):
            if current_session() is None:
                session.clear()
                flash("Sign in to continue.", "error")
                return redirect(url_for("login"))
            return view(*args, **kwargs)

        return wrapped  # type: ignore[return-value]

    @app.get("/account")
    @login_required
    def account():
        current = current_session()
        assert current is not None
        return render_template("account.html", account_email=current.email)

    return app


class PortalError(Exception):
    def __init__(self, message: str, status: int = 502) -> None:
        super().__init__(message)
        self.message = message
        self.status = status


def csrf_token() -> str:
    token = session.get("csrf_token")
    if not token:
        token = secrets.token_urlsafe(32)
        session["csrf_token"] = token
    return token


def validate_nakama_url(value: str) -> str:
    url = value.rstrip("/")
    try:
        parsed = urlsplit(url)
        host = parsed.hostname
    except ValueError:
        raise PortalError("Nakama URL configuration is invalid.", 500) from None
    try:
        is_loopback_ip = bool(host) and ipaddress.ip_address(host).is_loopback
    except ValueError:
        is_loopback_ip = False
    if (
        parsed.scheme not in {"http", "https"}
        or not host
        or parsed.username is not None
        or parsed.password is not None
        or parsed.query
        or parsed.fragment
        or parsed.path not in {"", "/"}
        or (parsed.scheme == "http" and not is_loopback_ip)
    ):
        raise PortalError("Nakama must use HTTPS, except for a literal loopback development URL.", 500)
    return url


def is_loopback_url(value: str) -> bool:
    try:
        host = urlsplit(value).hostname
        return bool(host) and ipaddress.ip_address(host).is_loopback
    except ValueError:
        return False


def configured_server_key(app: Flask, base_url: str) -> str:
    server_key = app.config["NAKAMA_SERVER_KEY"]
    if not server_key:
        raise PortalError("Configure NAKAMA_SERVER_KEY in account_portal/.env before use.", 500)
    if server_key == "defaultkey" and not is_loopback_url(base_url):
        raise PortalError("Replace Nakama's default server key before using a remote server.", 500)
    return server_key


def authenticate_email(
    app: Flask,
    *,
    email: str,
    password: str,
    create: bool,
    username: str | None,
) -> NakamaSession:
    base_url = validate_nakama_url(app.config["NAKAMA_URL"])
    server_key = configured_server_key(app, base_url)
    query: dict[str, str] = {"create": str(create).lower()}
    if username:
        query["username"] = username
    endpoint = f"{base_url}/v2/account/authenticate/email?{urlencode(query)}"
    payload = {"email": email, "password": password}

    try:
        with requests.Session() as http:
            http.trust_env = False
            response = http.post(
                endpoint,
                json=payload,
                auth=(server_key, ""),
                timeout=REQUEST_TIMEOUT_SECONDS,
                allow_redirects=False,
                stream=True,
            )
    except requests.RequestException:
        raise PortalError("Could not reach Nakama. Check the server URL and try again.", 503) from None

    try:
        if response.status_code in {401, 403}:
            raise PortalError("Email or password was not accepted.", 401)
        if response.status_code == 409:
            raise PortalError("That account could not be created. It may already exist.", 409)
        if response.status_code == 429:
            raise PortalError("Too many attempts. Wait a moment and try again.", 429)
        if response.status_code < 200 or response.status_code >= 300:
            raise PortalError("Nakama could not complete the account request.", 502)
        chunks: list[bytes] = []
        response_size = 0
        for chunk in response.iter_content(chunk_size=8192):
            if not chunk:
                continue
            response_size += len(chunk)
            if response_size > MAX_RESPONSE_BYTES:
                raise PortalError("Nakama returned an invalid response.", 502)
            chunks.append(chunk)
        body = json.loads(b"".join(chunks))
        access = body["token"]
        refresh = body["refresh_token"]
    except requests.RequestException:
        raise PortalError("Could not read Nakama's authentication response.", 502) from None
    except (ValueError, KeyError, TypeError):
        raise PortalError("Nakama returned an invalid authentication response.", 502) from None
    finally:
        response.close()
    if not isinstance(access, str) or not access or not isinstance(refresh, str) or not refresh:
        raise PortalError("Nakama returned an invalid authentication response.", 502)
    return NakamaSession(access, refresh, email)


def revoke_session(app: Flask, nakama_session: NakamaSession) -> None:
    base_url = validate_nakama_url(app.config["NAKAMA_URL"])
    try:
        with requests.Session() as http:
            http.trust_env = False
            response = http.post(
                f"{base_url}/v2/session/logout",
                json={"token": nakama_session.access_token, "refreshToken": nakama_session.refresh_token},
                headers={"Authorization": f"Bearer {nakama_session.access_token}"},
                timeout=REQUEST_TIMEOUT_SECONDS,
                allow_redirects=False,
                stream=True,
            )
    except requests.RequestException:
        raise PortalError("Nakama logout could not be confirmed.") from None
    try:
        if response.status_code < 200 or response.status_code >= 300:
            raise PortalError("Nakama logout could not be confirmed.")
    finally:
        response.close()


def establish_session(nakama_session: NakamaSession, email: str) -> None:
    remove_session()
    session.clear()
    sid = secrets.token_urlsafe(32)
    sessions = _session_store()
    with _session_lock():
        sessions[sid] = nakama_session
    session["sid"] = sid
    session["email"] = email
    session["csrf_token"] = secrets.token_urlsafe(32)
    session.permanent = True


def current_session() -> NakamaSession | None:
    sid = session.get("sid")
    if not isinstance(sid, str):
        return None
    sessions = _session_store()
    with _session_lock():
        return sessions.get(sid)


def remove_session() -> None:
    sid = session.get("sid")
    if isinstance(sid, str):
        with _session_lock():
            _session_store().pop(sid, None)


def _session_store() -> dict[str, NakamaSession]:
    from flask import current_app

    return current_app.extensions["nakama_sessions"]


def _session_lock() -> threading.RLock:
    from flask import current_app

    return current_app.extensions["nakama_sessions_lock"]


app = create_app()


def main() -> None:
    try:
        port = int(os.environ.get("PORT", "5050"))
    except ValueError:
        raise SystemExit("PORT must be an integer from 1 through 65535.") from None
    if not 1 <= port <= 65535:
        raise SystemExit("PORT must be an integer from 1 through 65535.")
    host = os.environ.get("HOST", "127.0.0.1")
    if host not in {"127.0.0.1", "::1", "localhost"}:
        print("Warning: Flask's development server is not suitable for public exposure.")
    app.run(host=host, port=port, debug=False)


if __name__ == "__main__":
    main()
