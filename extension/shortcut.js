/*
 * The global "new note" shortcut.
 *
 * A Wayland client cannot grab a global accelerator, but the compositor can, so
 * the keybinding lives here and pokes the app over D-Bus. Activating the app's
 * action rather than spawning the binary means an already-running instance is
 * reused, and a stopped one is started by D-Bus activation.
 */

import Gio from 'gi://Gio';
import GLib from 'gi://GLib';
import Meta from 'gi://Meta';
import Shell from 'gi://Shell';

import * as Main from 'resource:///org/gnome/shell/ui/main.js';

import { APP_BUS_NAME, APP_OBJECT_PATH } from './interface.js';

export class GlobalShortcut {
    constructor(settings) {
        this._settings = settings;
        this._bound = false;
        this._bind();

        // Rebind when the user changes the accelerator in dconf.
        this._changedId = settings.connect('changed::new-note', () => {
            this._unbind();
            this._bind();
        });
    }

    destroy() {
        this._settings.disconnect(this._changedId);
        this._changedId = 0;
        this._unbind();
        this._settings = null;
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
                    console.error(`Stickies: could not create a note: ${error.message}`);
                }
            }
        );
    }
}
