# Development

The mobile crate defines the entrypoint for the mobile app along with any assets, components and dependencies that are specific to mobile builds. The mobile crate starts out something like this:

```
mobile/
├─ assets/ # Assets used by the mobile app - Any platform specific assets should go in this folder
├─ src/
│  ├─ main.rs # The entrypoint for the mobile app.It also defines the routes for the mobile platform
│  ├─ views/ # The views each route will render in the mobile version of the app
│  │  ├─ mod.rs # Defines the module for the views route and re-exports the components for each route
│  │  ├─ blog.rs # The component that will render at the /blog/:id route
│  │  ├─ home.rs # The component that will render at the / route
├─ Cargo.toml # The mobile crate's Cargo.toml - This should include all mobile specific dependencies
```

## Dependencies

This crate is included only in mobile builds. Add mobile-specific dependencies to this crate's [Cargo.toml](Cargo.toml), and put reusable components in the shared [ui](../ui/Cargo.toml) crate.

### Serving Your Mobile App

Mobile platforms are shared in a single crate. To serve mobile, you need to explicitly set your target device to `android` or `ios`:

```bash
dx serve --package mobile --platform android
```
