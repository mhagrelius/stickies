/*
 * The D-Bus surface Stickies calls.
 *
 * Every method resolves a window by the object path GTK publishes for its
 * application windows (delivered to Mutter over the gtk_shell1 protocol), and
 * refuses to act on any window whose path does not sit under Stickies' own
 * prefix. This service therefore cannot move, resize or raise anyone else's
 * windows, even for a caller that has found the interface and guessed a path.
 */

import Gio from 'gi://Gio';
import Meta from 'gi://Meta';

import * as Main from 'resource:///org/gnome/shell/ui/main.js';

import { clampToWorkArea, monitorEntry, windowEntry } from './geometry.js';
import {
    boolReply,
    INTERFACE_XML,
    listMonitorsReply,
    OBJECT_PATH,
    PROTOCOL_VERSION,
    queryAllReply,
    queryReply,
    BUS_NAME,
    WINDOW_PATH_PREFIX,
} from './interface.js';

export class StickiesService {
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
        Main.layoutManager.disconnect(this._monitorsChangedId);
        this._monitorsChangedId = 0;

        Gio.bus_unown_name(this._nameId);
        this._nameId = 0;

        this._dbus.unexport();
        this._dbus = null;
        this._hider = null;
    }

    /** The hider is rebuilt when its setting changes; the service follows it. */
    setHider(hider) {
        this._hider = hider;
    }

    get Version() {
        return PROTOCOL_VERSION;
    }

    // ---- D-Bus methods --------------------------------------------------

    ListMonitors() {
        const manager = this._monitorManager();
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
                monitor.get_display_name(),
                index === primary,
                area
            );
            if (entry)
                monitors.push(entry);
        }

        return listMonitorsReply(monitors);
    }

    Place(objectPath, connector, x, y, width, height) {
        const window = this._findWindow(objectPath);
        if (!window)
            return boolReply(false);

        const index = this._monitorIndexFor(connector);
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
            const path = window?.get_gtk_window_object_path();
            if (!path?.startsWith(WINDOW_PATH_PREFIX))
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
        return global.backend.get_monitor_manager();
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
            if (window.get_gtk_window_object_path() === objectPath)
                return window;
        }
        return null;
    }

    _monitorIndexFor(connector) {
        const index = this._monitorManager().get_monitor_for_connector(connector);
        if (index >= 0)
            return index;

        // The remembered monitor is gone. The app already prefers the primary
        // in that case, but a layout change between its ListMonitors call and
        // this one can still land here.
        return global.display.get_primary_monitor();
    }

    _connectorForIndex(index) {
        const manager = this._monitorManager();
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

        const area = Main.layoutManager.getWorkAreaForMonitor(index);
        return windowEntry(connector, area, window.get_frame_rect());
    }
}
