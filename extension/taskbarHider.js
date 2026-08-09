/*
 * Keep note windows out of the dash, the window switcher and the overview.
 *
 * Sticky notes behave more like desktop widgets than application windows, and
 * having eight of them in Alt-Tab is noise.
 *
 * `Meta.Window:skip-taskbar` is a read-only *property*, so assigning to it
 * silently does nothing — which is exactly what happened here first time.
 * Mutter provides methods for it instead: `hide_from_window_list()` and
 * `show_in_window_list()`. They exist precisely because Wayland has no
 * client-side equivalent (GTK 4 dropped `set_skip_taskbar_hint`, which was
 * only ever implemented for X11).
 */

import { WINDOW_PATH_PREFIX } from './interface.js';

/** Whether a window is one of ours, by the object path GTK gave it. */
function isNote(window) {
    const path = window?.get_gtk_window_object_path();
    return Boolean(path?.startsWith(WINDOW_PATH_PREFIX));
}

export class TaskbarHider {
    constructor() {
        this._createdId = global.display.connect('window-created', (_display, window) =>
            this.apply(window)
        );
        for (const actor of global.get_window_actors())
            this.apply(actor.meta_window);
    }

    destroy() {
        global.display.disconnect(this._createdId);
        this._createdId = 0;

        // Put them back: an extension must leave the session as it found it,
        // and a note stuck out of the dock after uninstalling would be a
        // genuinely confusing thing to be left with.
        for (const actor of global.get_window_actors()) {
            const window = actor.meta_window;
            if (isNote(window))
                window.show_in_window_list();
        }
    }

    /** @returns {boolean} whether the window is now hidden from window lists. */
    apply(window) {
        if (!isNote(window))
            return false;

        window.hide_from_window_list();
        return window.is_skip_taskbar();
    }
}
