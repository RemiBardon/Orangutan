## Test cases

| HTTP req               | target file            | Trimming prefix | On the server                   |
| ---------------------- | ---------------------- | --------------- | ------------------------------- |
| `/whatever`            | `/whatever/index.html` | `"/index.html"` | `/whatever/index.html@_default` |
| `/whatever/`           | `/whatever/index.html` | `"index.html"`  | `/whatever/index.html@_default` |
| `/whatever/index.html` | `/whatever/index.html` | `""`            | `/whatever/index.html@_default` |
| `/style.css`           | `/style.css`           | `""`            | `/style.css@_default`           |
| `/anything.custom`     | `/anything.custom`     | `""`            | `/anything.custom`              |

Let's say on the server there is:

- `/a/index.html@family`
- `/a/index.html@friends`
- `/a/b/index.html@_default`

| HTTP req        | target file     | List matching objects                                                           | Trimming prefix                                                           |
| --------------- | --------------- | ------------------------------------------------------------------------------- | ------------------------------------------------------------------------- |
| `/a`            | `/a/index.html` | `["/a/index.html@family", "/a/index.html@friends", "/a/b/index.html@_default"]` | `["/index.html@family", "/index.html@friends", "/b/index.html@_default"]` |
| `/a/`           | `/a/index.html` | `["/a/index.html@family", "/a/index.html@friends", "/a/b/index.html@_default"]` | `["index.html@family", "index.html@friends", "b/index.html@_default"]`    |
| `/a/index.html` | `/a/index.html` | `["/a/index.html@family", "/a/index.html@friends"]`                             | `["@family", "@friends"]`                                                 |
