#!/usr/bin/python3
"""Optional GNOME AppIndicator. All sync operations belong to the Rust service."""
import json
import os
import subprocess
import gi
gi.require_version("Gtk", "3.0")
gi.require_version("AyatanaAppIndicator3", "0.1")
from gi.repository import Gtk, Gio, GLib, AyatanaAppIndicator3

BUS = "io.github.zowber.Peedrive.Sync"
OBJECT = "/io/github/zowber/Peedrive"


def summary(state):
    """Title and icon for the current state, most urgent first."""
    mappings = state.get("mappings", [])
    states = [m["state"] for m in mappings]
    if state.get("account_error") or "Needs attention" in states:
        return "Sync needs attention", "dialog-warning-symbolic"
    if not state.get("signed_in") or "Signed out" in states:
        return "Sign in to keep syncing", "dialog-warning-symbolic"
    if "Review changes" in states or state.get("conflicts"):
        return "Changes need your review", "dialog-warning-symbolic"
    if "Offline" in states:
        return "Offline · will sync when connected", "network-offline-symbolic"
    for m in mappings:
        if m["state"] == "Syncing":
            total = max(m.get("pending", 0), 1)
            return f"Syncing {min(m.get('done', 0) + 1, total)} of {total} changes", "view-refresh-symbolic"
    if "Checking" in states or "Waiting to retry" in states:
        return "Checking for changes…", "view-refresh-symbolic"
    if any(m["mapping"]["paused"] for m in mappings):
        return "Sync paused", "media-playback-pause-symbolic"
    if not mappings:
        return "Nothing selected to sync", "folder-remote-symbolic"
    return "Everything is up to date", "folder-remote-symbolic"


class Indicator:
    def __init__(self):
        self.state = {}
        self.pending = False
        self.shown = None
        self.indicator = AyatanaAppIndicator3.Indicator.new(
            "peedrive", "folder-remote-symbolic", AyatanaAppIndicator3.IndicatorCategory.APPLICATION_STATUS)
        self.indicator.set_status(AyatanaAppIndicator3.IndicatorStatus.ACTIVE)
        self.indicator.set_title("Peedrive")
        self.proxy = Gio.DBusProxy.new_for_bus_sync(Gio.BusType.SESSION, Gio.DBusProxyFlags.NONE, None, BUS, OBJECT, BUS, None)
        self.build(("Connecting…", "folder-remote-symbolic"), [], False, False)
        GLib.timeout_add_seconds(2, self.refresh)
        self.refresh()

    def item(self, menu, text, callback=None):
        item = Gtk.MenuItem(label=text)
        if callback:
            item.connect("activate", callback)
        else:
            item.set_sensitive(False)
        menu.append(item)

    def build(self, title, recent, paused, has_mapping):
        menu = Gtk.Menu()
        self.item(menu, title[0])
        if recent:
            menu.append(Gtk.SeparatorMenuItem())
            for event in recent:
                arrow = "↑" if event["kind"] == "upload" else "↓"
                name = os.path.basename(event["path"])
                self.item(menu, f"{arrow}  {name}", lambda _, p=event["path"]: self.open_file(p))
        menu.append(Gtk.SeparatorMenuItem())
        if has_mapping:
            self.item(menu, "Open Proton Drive folder", lambda _: self.open_file(""))
            self.item(menu, "Resume syncing" if paused else "Pause syncing", self.toggle)
        self.item(menu, "Open Peedrive", self.open_app)
        menu.append(Gtk.SeparatorMenuItem())
        self.item(menu, "Quit Peedrive", self.quit)
        menu.show_all()
        self.indicator.set_menu(menu)
        self.indicator.set_icon_full(title[1], title[0])

    def root(self):
        mappings = self.state.get("mappings", [])
        return mappings[0] if mappings else None

    def open_file(self, relative):
        mapping = self.root()
        if mapping:
            path = os.path.join(mapping["mapping"]["local"], relative)
            Gio.AppInfo.launch_default_for_uri(Gio.File.new_for_path(path).get_uri(), None)

    def quit(self, *_):
        """Stop background sync and remove the tray until Peedrive is opened or the next login."""
        try:
            request = json.dumps({"action": "quit"})
            self.proxy.call_sync("Submit", GLib.Variant("(s)", (request,)), Gio.DBusCallFlags.NO_AUTO_START, 3000, None)
        except GLib.Error:
            pass
        Gtk.main_quit()

    def open_app(self, *_):
        subprocess.Popen(["peedrive-app"])

    def toggle(self, *_):
        mapping = self.root()
        if mapping:
            action = "resume" if mapping["mapping"]["paused"] else "pause"
            request = json.dumps({"action": action, "id": mapping["mapping"]["id"]})
            self.proxy.call("Submit", GLib.Variant("(s)", (request,)), Gio.DBusCallFlags.NONE, 3000, None, None, None)

    def refresh(self):
        if not self.pending:
            self.pending = True
            self.proxy.call("Status", None, Gio.DBusCallFlags.NONE, 3000, None, self.received, None)
        return True

    def received(self, proxy, result, _):
        self.pending = False
        try:
            self.state = json.loads(proxy.call_finish(result).unpack()[0])
            title = summary(self.state)
        except (GLib.Error, ValueError):
            self.state = {}
            title = ("Sync service unavailable", "dialog-warning-symbolic")
        recent = [e for e in self.state.get("activity", []) if e.get("kind") in ("upload", "download") and e.get("path")][:3]
        mapping = self.root()
        shown = (title, [e["id"] for e in recent], bool(mapping and mapping["mapping"]["paused"]), bool(mapping))
        if shown != self.shown:
            self.shown = shown
            self.build(title, recent, shown[2], shown[3])


if __name__ == "__main__":
    indicator = Indicator()
    Gtk.main()
