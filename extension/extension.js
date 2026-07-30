/*
 * Stickies Window Placement — GNOME Shell extension.
 *
 * The Stickies app cannot position its own windows: Wayland has no client-side
 * positioning request, Mutter does not implement wlr-layer-shell, and GTK 4
 * removed gtk_window_move. This extension runs inside the compositor, where
 * those operations are available, and exposes exactly the handful of them that
 * Stickies needs on the session bus.
 *
 * Scope is deliberately narrow. Every method resolves a window by the D-Bus
 * object path GTK publishes for its application windows (delivered to Mutter
 * over the gtk_shell1 protocol), and refuses to act on any window whose path
 * does not sit under Stickies' own prefix. The extension therefore cannot move,
 * resize or raise anyone else's windows, even for a caller that has found the
 * interface and guessed a path.
 *
 * The arithmetic lives in geometry.js and the contract in interface.js, both
 * free of shell imports so they can be tested outside a live session — see
 * `gjs -m extension/test.js`.
 */

import Gio from 'gi://Gio';
import GLib from 'gi://GLib';
import Meta from 'gi://Meta';
import Shell from 'gi://Shell';

import {Extension} from 'resource:///org/gnome/shell/extensions/extension.js';
import * as Main from 'resource:///org/gnome/shell/ui/main.js';

import {clampToWorkArea, monitorEntry, windowEntry} from './geometry.js';
import {
    APP_BUS_NAME,
    APP_OBJECT_PATH,
    BUS_NAME,
    boolReply,
    INTERFACE_XML,
    listMonitorsReply,
    OBJECT_PATH,
    PROTOCOL_VERSION,
    queryAllReply,
    queryReply,
    WINDOW_PATH_PREFIX,
} from './interface.js';

class StickiesService {
    constructor(hider = null) {
        this._hider = hider;
        this._dbus = Gio.DBusExportedObject.wrapJSObject(INTERFACE_XML, this);
        this._dbus.export(Gio.DBus.session, OBJECT_PATH);

        this._nameId = Gio.bus_own_name(
            Gio.BusType.SESSION,
            BUS_NAME,
            Gio.BusNameOwnerFlags.NONE,
            null,
            null,
            null
        );

        this._monitorsChangedId = Main.layoutManager.connect(
            'monitors-changed',
            () => this._dbus.emit_signal('MonitorsChanged', null)
        );
    }

    destroy() {
        if (this._monitorsChangedId) {
            Main.layoutManager.disconnect(this._monitorsChangedId);
            this._monitorsChangedId = 0;
        }
        if (this._nameId) {
            Gio.bus_unown_name(this._nameId);
            this._nameId = 0;
        }
        this._dbus?.unexport();
        this._dbus = null;
    }

    get Version() {
        return PROTOCOL_VERSION;
    }

    // ---- D-Bus methods --------------------------------------------------

    ListMonitors() {
        const manager = this._monitorManager();
        if (!manager)
            return listMonitorsReply([]);

        const primary = global.display.get_primary_monitor();
        const monitors = [];

        for (const monitor of manager.get_monitors()) {
            if (!monitor.is_active())
                continue;

            const connector = monitor.get_connector();
            const index = manager.get_monitor_for_connector(connector);
            if (index < 0)
                continue;

            // Work area, not monitor geometry: notes must never open beneath
            // the top bar or the dock.
            const area = Main.layoutManager.getWorkAreaForMonitor(index);
            const entry = monitorEntry(
                connector,
                monitor.get_display_name?.(),
                index === primary,
                area
            );
            if (entry)
                monitors.push(entry);
            else
                log(`Stickies: skipping unusable monitor ${connector}`);
        }

        return listMonitorsReply(monitors);
    }

    Place(objectPath, connector, x, y, width, height) {
        const window = this._findWindow(objectPath);
        if (!window)
            return boolReply(false);

        const index = this._monitorIndexFor(connector);
        if (index < 0)
            return boolReply(false);

        const area = Main.layoutManager.getWorkAreaForMonitor(index);
        const rect = clampToWorkArea(area, x, y, width, height);

        // A maximised or tiled window ignores move_resize_frame; restore it
        // first so the geometry actually takes.
        if (window.is_maximized())
            window.unmaximize(Meta.MaximizeFlags.BOTH);

        // user_op = false: this is state restoration, not a drag, and should
        // not be recorded as the user having positioned the window by hand.
        window.move_resize_frame(false, rect.x, rect.y, rect.width, rect.height);
        return boolReply(true);
    }

    Query(objectPath) {
        const window = this._findWindow(objectPath);
        return queryReply(window ? this._geometryOf(window) : null);
    }

    QueryAll() {
        const windows = [];
        for (const actor of global.get_window_actors()) {
            const window = actor.meta_window;
            const path = window?.get_gtk_window_object_path?.();
            if (!path || !path.startsWith(WINDOW_PATH_PREFIX))
                continue;

            // Re-assert taskbar hiding here rather than only on
            // 'window-created'. Putting a note away and showing it again
            // re-maps the surface, and anything that rebuilds the window can
            // drop the flag; the app polls this every couple of seconds, so
            // making it self-healing costs nothing and removes a whole class
            // of "it came back in the dock" reports.
            this._hider?.apply(window);

            const geometry = this._geometryOf(window);
            if (geometry)
                windows.push([path, ...geometry]);
        }
        return queryAllReply(windows);
    }

    SetPinned(objectPath, pinned) {
        const window = this._findWindow(objectPath);
        if (!window)
            return boolReply(false);

        if (pinned)
            window.make_above();
        else
            window.unmake_above();

        return boolReply(true);
    }

    // ---- helpers --------------------------------------------------------

    _monitorManager() {
        const backend = global.backend ?? Meta.get_backend?.();
        return backend?.get_monitor_manager?.() ?? null;
    }

    /**
     * Resolve a window by its GTK D-Bus object path, refusing anything that is
     * not a Stickies note. Every public method goes through this.
     */
    _findWindow(objectPath) {
        if (typeof objectPath !== 'string' || !objectPath.startsWith(WINDOW_PATH_PREFIX))
            return null;

        for (const actor of global.get_window_actors()) {
            const window = actor.meta_window;
            if (!window || window.is_override_redirect())
                continue;
            if (window.get_gtk_window_object_path?.() === objectPath)
                return window;
        }
        return null;
    }

    _monitorIndexFor(connector) {
        const manager = this._monitorManager();
        if (!manager)
            return -1;

        const index = manager.get_monitor_for_connector(connector);
        if (index >= 0)
            return index;

        // The remembered monitor is gone. The app already prefers the primary
        // in that case, but a layout change between its ListMonitors call and
        // this one can still land here.
        return global.display.get_primary_monitor();
    }

    _connectorForIndex(index) {
        const manager = this._monitorManager();
        if (!manager)
            return null;

        for (const monitor of manager.get_monitors()) {
            const connector = monitor.get_connector();
            if (manager.get_monitor_for_connector(connector) === index)
                return connector;
        }
        return null;
    }

    /** `[connector, relX, relY, width, height]` for a mapped window. */
    _geometryOf(window) {
        const index = window.get_monitor();
        if (index < 0)
            return null;

        const connector = this._connectorForIndex(index);
        if (!connector)
            return null;

        const rect = window.get_frame_rect();

        const area = Main.layoutManager.getWorkAreaForMonitor(index);
        return windowEntry(connector, area, rect);
    }
}

/**
 * The global "new note" shortcut.
 *
 * A Wayland client cannot grab a global accelerator, but the compositor can,
 * so the keybinding lives here and pokes the app over D-Bus. Activating the
 * app's action rather than spawning the binary means an already-running
 * instance is reused, and a stopped one is started by D-Bus activation.
 */
class GlobalShortcut {
    constructor(settings) {
        this._settings = settings;
        this._bind();

        // Rebind when the user changes the accelerator in dconf.
        this._changedId = settings.connect('changed::new-note', () => {
            this._unbind();
            this._bind();
        });
    }

    destroy() {
        if (this._changedId) {
            this._settings.disconnect(this._changedId);
            this._changedId = 0;
        }
        this._unbind();
    }

    _bind() {
        if (this._settings.get_strv('new-note').length === 0)
            return; // Deliberately unbound.

        Main.wm.addKeybinding(
            'new-note',
            this._settings,
            Meta.KeyBindingFlags.NONE,
            Shell.ActionMode.NORMAL | Shell.ActionMode.OVERVIEW,
            () => this._newNote()
        );
        this._bound = true;
    }

    _unbind() {
        if (this._bound) {
            Main.wm.removeKeybinding('new-note');
            this._bound = false;
        }
    }

    _newNote() {
        Gio.DBus.session.call(
            APP_BUS_NAME,
            APP_OBJECT_PATH,
            'org.gtk.Actions',
            'Activate',
            // (s, av, a{sv}): action name, arguments, platform data.
            new GLib.Variant('(sava{sv})', ['new-note', [], {}]),
            null,
            Gio.DBusCallFlags.NONE,
            -1,
            null,
            (connection, result) => {
                try {
                    connection.call_finish(result);
                } catch (error) {
                    logError(error, 'Stickies: could not create a note');
                }
            }
        );
    }
}

/**
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
class TaskbarHider {
    constructor() {
        this._createdId = global.display.connect('window-created', (_display, window) =>
            this.apply(window)
        );
        for (const actor of global.get_window_actors())
            this.apply(actor.meta_window);
    }

    destroy() {
        if (this._createdId) {
            global.display.disconnect(this._createdId);
            this._createdId = 0;
        }
        // Put them back: an extension must leave the session as it found it,
        // and a note stuck out of the dock after uninstalling would be a
        // genuinely confusing thing to be left with.
        for (const actor of global.get_window_actors()) {
            const window = actor.meta_window;
            const path = window?.get_gtk_window_object_path?.();
            if (path?.startsWith(WINDOW_PATH_PREFIX))
                window.show_in_window_list?.();
        }
    }

    /** @returns {boolean} whether the window is now hidden from window lists. */
    apply(window) {
        const path = window?.get_gtk_window_object_path?.();
        if (!path || !path.startsWith(WINDOW_PATH_PREFIX))
            return false;

        if (typeof window.hide_from_window_list !== 'function') {
            // Older Mutter. Nothing breaks; notes just stay in the dock.
            log('Stickies: this Mutter cannot hide windows from the dock');
            return false;
        }

        window.hide_from_window_list();
        const hidden = window.is_skip_taskbar();
        if (!hidden)
            log(`Stickies: ${path} would not hide from the window list`);
        return hidden;
    }
}

export default class StickiesExtension extends Extension {
    enable() {
        // Wrapped because an exception here leaves the extension in ERROR with
        // only GJS's message to go on, and half of enable() may already have
        // run — so what got created has to be torn down.
        try {
            this._settings = this.getSettings();
            if (this._settings.get_boolean('hide-from-taskbar'))
                this._hider = new TaskbarHider();

            this._service = new StickiesService(this._hider);
            this._shortcut = new GlobalShortcut(this._settings);

            log('Stickies: extension enabled');
        } catch (error) {
            logError(error, 'Stickies: enable() failed');
            this.disable();
            throw error;
        }
    }

    disable() {
        // Unconditional teardown: the extension holds a bus name, a keybinding
        // and two signal handlers, and leaving any of them behind across a lock
        // or a shell restart leaks into the next session.
        this._hider?.destroy();
        this._hider = null;
        this._shortcut?.destroy();
        this._shortcut = null;
        this._service?.destroy();
        this._service = null;
        this._settings = null;
    }
}
