# MCP

**MCP** (Model Context Protocol) is a standard that lets an AI application
discover and call tools offered by another program. A tool might inspect a
project or validate content. MCP itself does not make a tool safe: each tool
still needs carefully limited permissions and input validation.

Lynx's planned MCP tools are optional and disabled by default. The intended
content tools reuse the CLI validators; write or publish actions require
explicit approval. See [AI tooling](../ai-tooling.md).
