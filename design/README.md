# Orangutan design specification

- [v1](v1/README.md) (pages stored encrypted in a file storage, retrieved and decrypted on the fly by a server handling authorization)
- [v2](v2/README.md) (always-running server generating static sites lazily based on a user's profile)
- [v3](v3/README.md) (v2 but profiles are not in the tokens but in the repository files)
- v4 (v3 with support for [Single Sign-On](https://en.wikipedia.org/wiki/Single_sign-on))

> [!NOTE]
> Orangutan is currently in v2, v3 development is on the way and v4 is more like a roadmap.

> [!NOTE]
> Those are "marketing versions", not [semantic versions](https://semver.org/).
> v2 was incompatible with v1, but v4 and v3 will be compatible with v2.
