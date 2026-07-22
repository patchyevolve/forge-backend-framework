# forge-proto

**This crate has been renamed to `forgecore-backend-framework-daemon`.**

This is a redirect crate for backwards compatibility. It re-exports the full
public API of `forgecore-backend-framework-daemon`.

## Migration

Update your `Cargo.toml`:

```toml
# Old
forge-proto = "1.0"

# New
forge = { package = "forgecore-backend-framework-daemon", version = "1.0" }
```

Your Rust source files stay the same — `use forge::proto::...` continues to work.
