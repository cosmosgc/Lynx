# Online client demo launchers

Open two PowerShell windows at the repository root.

In the first window, start the server:

```powershell
.\tests\client-online-demo\start-demo-server.bat
```

This launcher creates a fresh high-entropy token in the ignored local file
`tests/client-online-demo/demo-token.txt`. It gives only the token's SHA-256
hash to the loopback server, which listens on `127.0.0.1:8090`.

In the second window, start the client:

```powershell
.\tests\client-online-demo\start-demo-client.bat
```

The client launcher reads the token file and starts Cargo from the same
PowerShell process that sets the token. It uses
`ws://127.0.0.1:8090/world`. Stop either process with Ctrl+C. Delete the token
file after stopping both processes if you no longer need this demo setup.

This is a development-only demo. Use WASD to move, T to compose chat, Enter to
send and Escape to cancel chat. Movement pauses while composing or unfocused.
Transient disconnects retry automatically; R retries a terminal failure.
If connection fails, copy the detailed reason
from the window title when reporting the problem.
