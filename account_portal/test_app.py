from __future__ import annotations

import re
import unittest
from unittest.mock import Mock, patch

from app import PortalError, app as flask_app, create_app, main, validate_nakama_url


class AccountPortalTests(unittest.TestCase):
    def setUp(self) -> None:
        self.app = create_app(
            {
                "TESTING": True,
                "SECRET_KEY": "unit-test-secret-key-that-is-not-used-in-production",
                "NAKAMA_URL": "http://127.0.0.1:7350",
                "NAKAMA_SERVER_KEY": "test-public-key",
            }
        )
        self.client = self.app.test_client()

    @staticmethod
    def csrf(response) -> str:
        match = re.search(rb'name="csrf_token" value="([^"]+)"', response.data)
        if not match:
            raise AssertionError("CSRF token was not rendered")
        return match.group(1).decode()

    @staticmethod
    def nakama_http(status: int = 200) -> Mock:
        response = Mock()
        response.status_code = status
        response.iter_content.return_value = iter(
            [b'{"token":"private-access-token","refresh_token":"private-refresh-token"}']
        )
        http = Mock()
        http.__enter__ = Mock(return_value=http)
        http.__exit__ = Mock(return_value=False)
        http.post.return_value = response
        return http

    def test_registration_calls_nakama_and_keeps_tokens_out_of_browser_cookie(self) -> None:
        csrf = self.csrf(self.client.get("/register"))
        http = self.nakama_http()
        with patch("app.requests.Session", return_value=http):
            result = self.client.post(
                "/register",
                data={
                    "csrf_token": csrf,
                    "email": "player@example.test",
                    "username": "PlayerOne",
                    "password": "password-is-not-saved",
                },
            )

        self.assertEqual(result.status_code, 302)
        args, kwargs = http.post.call_args
        self.assertEqual(
            args[0],
            "http://127.0.0.1:7350/v2/account/authenticate/email?create=true&username=PlayerOne",
        )
        self.assertEqual(kwargs["json"], {
            "email": "player@example.test",
            "password": "password-is-not-saved",
        })
        self.assertEqual(kwargs["auth"], ("test-public-key", ""))
        self.assertFalse(http.trust_env)
        self.assertFalse(kwargs["allow_redirects"])
        cookie = result.headers["Set-Cookie"]
        self.assertNotIn("private-access-token", cookie)
        self.assertNotIn("private-refresh-token", cookie)
        account = self.client.get("/account")
        self.assertIn(b"player@example.test", account.data)
        self.assertNotIn(b"private-access-token", account.data)
        self.assertNotIn(b"private-refresh-token", account.data)

    def test_login_does_not_create_an_account(self) -> None:
        csrf = self.csrf(self.client.get("/login"))
        http = self.nakama_http()
        with patch("app.requests.Session", return_value=http):
            result = self.client.post(
                "/login",
                data={
                    "csrf_token": csrf,
                    "email": "player@example.test",
                    "password": "password-is-not-saved",
                },
            )
        self.assertEqual(result.status_code, 302)
        self.assertTrue(http.post.call_args.args[0].endswith("?create=false"))

    def test_post_without_csrf_token_is_rejected(self) -> None:
        result = self.client.post(
            "/login", data={"email": "player@example.test", "password": "secret"}
        )
        self.assertEqual(result.status_code, 400)

    def test_logout_revokes_session_and_clears_server_side_tokens(self) -> None:
        login_csrf = self.csrf(self.client.get("/login"))
        auth_http = self.nakama_http()
        with patch("app.requests.Session", return_value=auth_http):
            self.client.post(
                "/login",
                data={
                    "csrf_token": login_csrf,
                    "email": "player@example.test",
                    "password": "password-is-not-saved",
                },
            )

        logout_csrf = self.csrf(self.client.get("/account"))
        logout_http = Mock()
        logout_http.__enter__ = Mock(return_value=logout_http)
        logout_http.__exit__ = Mock(return_value=False)
        logout_response = Mock(status_code=200)
        logout_http.post.return_value = logout_response
        with patch("app.requests.Session", return_value=logout_http):
            response = self.client.post("/logout", data={"csrf_token": logout_csrf})

        self.assertEqual(response.status_code, 302)
        kwargs = logout_http.post.call_args.kwargs
        self.assertEqual(
            kwargs["headers"], {"Authorization": "Bearer private-access-token"}
        )
        self.assertEqual(
            kwargs["json"],
            {"token": "private-access-token", "refreshToken": "private-refresh-token"},
        )
        self.assertNotIn("auth", kwargs)
        self.assertEqual(self.client.get("/account").status_code, 302)

    def test_nakama_remote_http_is_rejected(self) -> None:
        with self.assertRaisesRegex(PortalError, "must use HTTPS"):
            validate_nakama_url("http://nakama.example.com:7350")
        self.assertEqual(validate_nakama_url("http://127.0.0.1:7350/"), "http://127.0.0.1:7350")
        self.assertEqual(validate_nakama_url("https://nakama.example.com"), "https://nakama.example.com")

    def test_app_reads_port_from_environment(self) -> None:
        with patch.dict("app.os.environ", {"PORT": "5123", "HOST": "127.0.0.1"}):
            with patch.object(flask_app, "run") as run:
                main()
        run.assert_called_once_with(host="127.0.0.1", port=5123, debug=False)


if __name__ == "__main__":
    unittest.main()
