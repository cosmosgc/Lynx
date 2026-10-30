# Lynx Account Portal

A small Flask web app for creating an email/password Nakama account and signing
in. It is a development portal, not a production internet-facing service.

## Setup

From this directory, create a virtual environment, install dependencies, and
copy the environment template:

```powershell
py -m venv .venv
.\.venv\Scripts\Activate.ps1
pip install -r requirements.txt
Copy-Item .env.example .env
```

Edit `account_portal/.env`. Set a long random `FLASK_SECRET_KEY` and the same
`NAKAMA_SERVER_KEY` configured by the local Nakama instance. Keep `.env`
untracked. Nakama's default `defaultkey` is accepted only with a literal
loopback URL for development; replace it before using a remote server.
`NAKAMA_URL` must use HTTPS, except that plain HTTP is allowed for a literal
loopback IP during local development.

Set `PORT` in this app-local `.env` to choose the listening port (default
`5050`). `HOST` defaults to `127.0.0.1`; keep it loopback for local development.

Start it with:

```powershell
python app.py
```

On Windows, `run.bat` creates the virtual environment and installs dependencies
on first use. Its first run copies `.env.example` to `.env` and stops so you can
set the Flask secret before starting the app.

Then open <http://127.0.0.1:5050>. Use **Create account** to register an email,
password, and optional username with Nakama, or **Sign in** for an existing
account. Nakama handles password storage and verification; this Flask app never
writes passwords to disk or logs.

## Session and security notes

- Nakama access and refresh tokens are held in server-process memory only. The
  browser cookie contains a signed opaque session ID, not tokens. Restarting the
  Flask process clears active sessions, so the user signs in again.
- Logout asks Nakama to revoke the session and always clears the local session.
- Requests bypass ambient HTTP proxies, do not follow redirects, have a timeout,
  and do not display Nakama response bodies to the user.
- CSRF tokens protect form submissions. Cookies are HttpOnly and SameSite=Lax;
  set `COOKIE_SECURE=true` only when serving behind HTTPS.
- Flask's built-in server is for local development only. Do not bind publicly or
  deploy this app as-is. Public deployment needs a production WSGI server, HTTPS,
  rate limiting, operational secret management, and a reviewed session store.
- The app provides account registration/login only; it does not yet create Lynx
  characters or provision a live world actor.

## Tests

```powershell
python -m unittest
```
