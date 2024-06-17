# Orangutan v2 design

## Terminology

### Profile

TODO

### Public/private

Files accessible without logging in are considered "public".
Those requiring at least one profile are considered "private".

## High level design

TODO

- Each user has a Biscuit token listing their profiles.
- Profiles are the only concept known by Orangutan, meaning users usually have one profile which is unique (their identity) and `0..*` profiles which are shared profiles (groups).
- Orangutan has no database.
- When applicable, refresh tokens are merged into an existing token to extend permissions (i.e. add profiles).
- Always-running server which subsribes to content changes.
- Lazily generated websites, with minimal number of copies.

## Processes

TODO

## Tests

TODO

## Known issues

### Leakage of page existence

**Problem:**

- Pages listing tags or categories have to be disabled to avoid leaking information.
- RSS feeds leak page existence and content.
- `index.json` in [Hugo](https://gohugo.io/) leaks private pages content.

**Solution:**

<!-- TODO -->

We will think about this later. There is certainly a solution.
