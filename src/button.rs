use std::{cell::RefCell, path::PathBuf};

use waybar_cffi::gtk::{
    self as gtk, Border, CssProvider, IconLookupFlags, IconSize, IconTheme, ReliefStyle,
    StateFlags,
    gdk_pixbuf::Pixbuf,
    prelude::{ButtonExt, CssProviderExt, GdkPixbufExt, IconThemeExt, StyleContextExt, WidgetExt},
};

use crate::state::State;

/// A taskbar button.
#[derive(Debug)]
pub struct Button {
    app_id: Option<String>,
    button: gtk::Button,
    state: State,
}

// These have to be declared as thread locals because Gtk objects are (generally) not Send.
// Practically, we're likely to be doing everything from the main thread anyway, but Glib can
// figure that out.
thread_local! {
    static BUTTON_CSS_PROVIDER: CssProvider = {
        let css = CssProvider::new();
        if let Err(e) = css.load_from_data(include_bytes!("style.css")) {
            eprintln!("css parse error: {e:?}");
        }

        css
    };

    static ICON_THEME: IconTheme = {
        IconTheme::default().unwrap_or_default()
    }
}

impl Button {
    /// Instantiates a new button, including creating a new Gtk button internally.
    pub fn new(state: &State, window: &niri_ipc::Window) -> Self {
        let state = state.clone();

        // Set up the basic image button.
        //
        // Note that we don't actually set the image here: we need to know the size before doing so
        // in order to load the most appropriate icon from the icon theme, and we won't know that
        // until we get an actual size allocation.
        let button = gtk::Button::new();
        button.set_always_show_image(true);
        button.set_relief(ReliefStyle::None);

        // Provide the base CSS for each button that users can then extend.
        BUTTON_CSS_PROVIDER.with(|provider| {
            button
                .style_context()
                .add_provider(provider, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
        });

        let app_id = window.app_id.clone();
        let icon_path = app_id
            .as_deref()
            .and_then(|id| state.icon_cache().lookup(id));

        let button = Self {
            app_id,
            button,
            state,
        };

        // Set up our event handlers. It's easier to do this with self already available.
        button.connect_click_handler(window.id);
        button.connect_size_allocate(icon_path);

        button
    }

    /// Sets whether the window represented by this button is currently focused.
    pub fn set_focus(&self, focus: bool) {
        let context = self.button.style_context();

        if focus {
            context.add_class("focused");
            context.remove_class("urgent");
        } else {
            context.remove_class("focused");
        }
    }

    /// Sets the window title.
    pub fn set_title(&self, title: Option<&str>) {
        self.button.set_tooltip_text(title);

        // Apply any app styling rules.
        if let Some(app_id) = &self.app_id {
            if let Some(title) = title {
                let config = self.state.config();
                let context = self.button.style_context();

                // First, remove all the possible classes for this app.
                for class in config.app_classes(app_id) {
                    context.remove_class(class);
                }

                // Now add the classes that actually do match.
                for class in config.app_matches(app_id, title) {
                    context.add_class(class);
                }
            }
        }
    }

    /// Sets the window to urgent: that is, needing attention.
    ///
    /// This state is automatically cleared the next time the window is focused.
    pub fn set_urgent(&self) {
        self.button.style_context().add_class("urgent");
    }

    /// Returns the actual [`gtk::Button`] widget.
    pub fn widget(&self) -> &gtk::Button {
        &self.button
    }

    fn connect_click_handler(&self, window_id: u64) {
        let state = self.state.clone();

        self.button.connect_clicked(move |_| {
            if let Err(e) = state.niri().activate_window(window_id) {
                eprintln!("error trying to activate window {}: {e:?}", window_id);
            }
        });
    }

    fn connect_size_allocate(&self, icon_path: Option<PathBuf>) {
        let last_size = RefCell::new(None);

        self.button
            .connect_size_allocate(move |button, allocation| {
                // Figure out if we actually need to redraw, since it's relatively expensive.
                //
                // The first condition is pretty easy: is there an image on the button? If not,
                // then it's the first draw, and we have no choice but to draw.
                let mut must_redraw = button.image().is_none();

                // Otherwise, let's check if the size allocation has changed since the last time
                // this was called.
                if !must_redraw {
                    if let Some(last_size) = last_size.take() {
                        if &last_size != allocation {
                            must_redraw = true;
                        }
                    } else {
                        must_redraw = true;
                    }

                    last_size.replace(Some(*allocation));
                }

                if must_redraw {
                    // Calculate the actual image size we need.
                    //
                    // Gtk3 doesn't provide a useful way to get the actual inner size of the
                    // element after applying style rules, so we have to do that here, otherwise we
                    // may draw the image too big and cause the container to grow. (Which will then
                    // result in another size allocate signal, which will result in another
                    // recalculation, which then results in your taskbar taking up your entire
                    // display within a few seconds.)
                    //
                    // Blindly using StateFlags::NORMAL probably isn't actually the right
                    // behaviour, but it's the best we've got for now.
                    //
                    // Note that we have to do this _after_ we figure out if we need to redraw:
                    // calculating the style information is apparently expensive enough that Gtk
                    // essentially busy-waits, which (a) burns CPU, and (b) means that :hover
                    // styles don't get applied. What that means in practice is that, if waybar's
                    // dynamically reloading CSS feature is enabled, sizing changes won't be
                    // applied after the button is first rendered.
                    //
                    // That seems to be the price we have to pay, though, so here we are.
                    let context = button.style_context();
                    let border = context.border(StateFlags::NORMAL);
                    let margin = context.margin(StateFlags::NORMAL);
                    let padding = context.padding(StateFlags::NORMAL);

                    let size = allocation.height()
                        - border.vertical_size()
                        - margin.vertical_size()
                        - padding.vertical_size();

                    // Now we know the size, we can actually load the image.
                    let image =
                        Self::icon_image(icon_path.as_ref(), button, size).unwrap_or_else(|| {
                            // If we can't find an application icon, then we need to use a
                            // fallback.
                            static FALLBACK_ICON: &str = "application-x-executable";

                            // We'll try to look the icon up in the default icon theme, since then
                            // we can load up the actual image and control its scaling and display.
                            ICON_THEME
                                .with(|theme| {
                                    theme.lookup_icon_for_scale(
                                        FALLBACK_ICON,
                                        size,
                                        button.scale_factor(),
                                        IconLookupFlags::empty(),
                                    )
                                })
                                .and_then(|info| {
                                    Self::icon_image(info.filename().as_ref(), button, size)
                                })
                                .unwrap_or_else(|| {
                                    // But, if all else fails, we'll just use the default button
                                    // size and YOLO it.
                                    gtk::Image::from_icon_name(
                                        Some(FALLBACK_ICON),
                                        IconSize::Button,
                                    )
                                })
                        });

                    // Finally, we can set the button image. Doing this from the callback doesn't
                    // seem to work reliably for reasons I don't understand at all, but doing it
                    // from the main loop as soon as possible does. :shrug:
                    let button = button.clone();
                    gtk::glib::source::idle_add_local_once(move || {
                        button.set_image(Some(&image));
                    });
                }
            });
    }

    fn icon_image(
        icon_path: Option<&PathBuf>,
        button: &gtk::Button,
        size: i32,
    ) -> Option<gtk::Image> {
        let size = size * button.scale_factor();

        icon_path
            .and_then(
                |path| match Pixbuf::from_file_at_scale(path, size, size, true) {
                    Ok(pixbuf) => Some(pixbuf),
                    Err(e) => {
                        eprintln!("cannot load icon at {path:?}: {e:?}");
                        None
                    }
                },
            )
            .and_then(|pixbuf| pixbuf.create_surface(0, button.window().as_ref()))
            .map(|surface| gtk::Image::from_surface(Some(&surface)))
    }
}

trait BorderExt {
    fn vertical_size(&self) -> i32;
}

impl BorderExt for Border {
    fn vertical_size(&self) -> i32 {
        (self.top + self.bottom).into()
    }
}
