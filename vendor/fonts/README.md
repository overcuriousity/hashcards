# Fonts

Two families, subset to the weights the stylesheet asks for, served from
`/fonts/{name}` by `src/cmd/drill/fonts.rs`. Vendored rather than fetched
from a CDN for the same reason KaTeX and highlight.js are: the server is
expected to work on a machine with no route to the internet.

- `inter-400.woff2`, `inter-500.woff2`, `inter-600.woff2` — Inter,
  <https://github.com/rsms/inter>, SIL Open Font License 1.1.
- `jetbrains-mono-400.woff2` — JetBrains Mono,
  <https://github.com/JetBrains/JetBrainsMono>, SIL Open Font License 1.1.
