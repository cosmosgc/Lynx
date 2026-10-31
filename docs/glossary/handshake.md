# Handshake and version negotiation

A **handshake** is the initial exchange when a client connects to a server. The
client and server identify themselves and agree on basic connection details
before normal gameplay messages begin.

Lynx's handshake includes protocol, engine, and content versions, plus named
features and locale. **Version negotiation** checks compatibility and decides
which advertised features both sides understand. It does not make an
unsupported feature safe or grant gameplay authority; the server still validates
each action. See [Networking: protocol versioning](../networking.md#protocol-versioning).
