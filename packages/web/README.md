# Development

The web crate defines the entrypoint for the web app along with any assets, components and dependencies that are specific to web builds. The web crate starts out something like this:

```
web/
├─ assets/ # Assets used by the web app - Any platform specific assets should go in this folder
├─ src/
│  ├─ main.rs # The entrypoint for the web app.It also defines the routes for the web platform
│  ├─ views/ # The views each route will render in the web version of the app
│  │  ├─ mod.rs # Defines the module for the views route and re-exports the components for each route
│  │  ├─ blog.rs # The component that will render at the /blog/:id route
│  │  ├─ home.rs # The component that will render at the / route
├─ Cargo.toml # The web crate's Cargo.toml - This should include all web specific dependencies
```

## Dependencies

This crate is included only in the web build. Add web-specific dependencies to this crate's [Cargo.toml](Cargo.toml), and put reusable components in the shared [ui](../ui/Cargo.toml) crate.

### Serving Your Web App

You can start your web app with the following command:

```bash
dx serve --package web --platform web --hot-reload true --addr 0.0.0.0
```
