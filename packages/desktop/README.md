# Development

The desktop crate defines the entrypoint for the desktop app along with any assets, components and dependencies that are specific to desktop builds. The desktop crate starts out something like this:

```
desktop/
├─ assets/ # Assets used by the desktop app - Any platform specific assets should go in this folder
├─ src/
│  ├─ main.rs # The entrypoint for the desktop app.It also defines the routes for the desktop platform
│  ├─ views/ # The views each route will render in the desktop version of the app
│  │  ├─ mod.rs # Defines the module for the views route and re-exports the components for each route
│  │  ├─ blog.rs # The component that will render at the /blog/:id route
│  │  ├─ home.rs # The component that will render at the / route
├─ Cargo.toml # The desktop crate's Cargo.toml - This should include all desktop specific dependencies
```

## Dependencies

This crate is included only in the desktop build. Add desktop-specific dependencies to this crate's [Cargo.toml](Cargo.toml), and put reusable components in the shared [ui](../ui/Cargo.toml) crate.

### Serving Your Desktop App

You can start your desktop app with the following command:

```bash
dx serve --package desktop --platform desktop
```
