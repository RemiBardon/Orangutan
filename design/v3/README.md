# Orangutan v3 design

## What's new?

- Migration to [JWT]s
  - Usage of [IANA-registered JWT claims]
  - JWTs are encrypted (to avoid data leaks)
  - Automatically migrate [Orangutan v2](../v2/README.md) Biscuits to JWTs when received
    - Feature disabled by default (to reduce the number of dependencies), which can be enabled using a [feature flag](https://doc.rust-lang.org/cargo/reference/features.html)
- Tokens don't contain profiles anymore, they are now exclusively used to identify users
  - Profiles are defined in the static site repository, as Orangutan still doesn't have a database
    - See [Orangutan data file format](#orangutan-data-file-format)
- Orangutan exposes UIs to the users
  - Users can generate links to connect their other devices later
    - Short lifetime (≈5 minutes)
    - Changes "Issuer" and "Issued At" claims in the JWT
    - Mention "don't share to anyone else"
      - We can't allow one to invite someone else as user profiles are defined in the [data file](#orangutan-data-file-format)
  - Orangutan allows usage of templating in the static pages to add user-sepcific information (i.e. full name)

## Orangutan data file format

- Tree-like structure
- Generic name so the file can be used for more than just defining profiles (e.g. runtime config or token revocation)
  - Something like `Orangutan.yaml`
  - Could allow `toml` or `json` too (using [`config-file`](https://crates.io/crates/config-file) to make it transparent)

```yaml
config:
  website_repository: git@github.com:RemiBardon/blog.git
  website_root: http://localhost:8080
profiles:
  # famille: {}
  # amis: {}
  amis-proches:
    inherits: [amis] # Doesn't have to be defined
  collegues: {}
  clever-cloud:
    inherits: [collegues]
  prose:
    inherits: [collegues]
  imt: {}
users:
  remi:
    inherits: ['*'] # Special profile
  papa:
    inherits: [famille] # Doesn't have to be defined
  valerian:
    inherits: [prose]
```

## TODO

- Orangutan communicates with the outside world
  - New content notifications
    - Define per-user notification channels in the [data file](#orangutan-data-file-format) (i.e. WhatsApp, email…)
    - Don't send notifications automatically as there could be a lot of unwanted false-positives (e.g. existing pages which changed URL)
      - Expose an admin route for sending notifications
        - List pages and send a single notification saying "All those pages have been published"
      - Add a templating key which adds a button if an admin is logged in, allowing them to automatically send a notification to all subscribed users which can read the current page

[JWT]: https://jwt.io/ "JSON Web Tokens - jwt.io"
[IANA-registered JWT claims]: https://www.iana.org/assignments/jwt/jwt.xhtml "JSON Web Token (JWT)"
