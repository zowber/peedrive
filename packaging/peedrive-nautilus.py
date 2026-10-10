"""GNOME Files emblems and menu for Peedrive. The sync service answers over D-Bus."""
import json
import os
import subprocess
import gi
gi.require_version("Nautilus", "4.1")
from gi.repository import GObject, Nautilus, Gio, GLib

BUS = "io.github.zowber.Peedrive.Sync"
OBJECT = "/io/github/zowber/Peedrive"
EMBLEMS = {"synced": "emblem-default", "syncing": "emblem-synchronizing",
           "paused": "emblem-synchronizing", "attention": "emblem-important"}
REMEMBERED = 5000


class PeedriveExtension(GObject.GObject, Nautilus.InfoProvider, Nautilus.MenuProvider):
    def __init__(self):
        super().__init__()
        self.mappings = []
        self.states = {}   # path -> state from the service
        self.files = {}    # path -> file info shown recently, oldest first
        self.unknown = set()
        self.busy = False
        self.proxy = Gio.DBusProxy.new_for_bus_sync(Gio.BusType.SESSION, Gio.DBusProxyFlags.DO_NOT_AUTO_START,
                                                    None, BUS, OBJECT, BUS, None)
        GLib.timeout_add_seconds(3, self.refresh)

    def call(self, method, args, done):
        self.proxy.call(method, args, Gio.DBusCallFlags.NO_AUTO_START, 5000, None, done, None)

    def refresh(self):
        """Re-ask for everything shown recently; emblems change as sync progresses."""
        if not self.busy and self.proxy.get_name_owner():
            self.busy = True
            self.unknown.clear()
            self.call("Status", None, self.received_status)
        return True

    def received_status(self, proxy, result, _):
        try:
            self.mappings = json.loads(proxy.call_finish(result).unpack()[0]).get("mappings", [])
        except (GLib.Error, ValueError):
            self.mappings = []
        self.ask(list(self.files))

    def ask(self, paths):
        if not paths:
            self.busy = False
            return
        self.call("FileStates", GLib.Variant("(as)", (paths,)), lambda p, r, _: self.received_states(p, r, paths))

    def received_states(self, proxy, result, paths):
        self.busy = False
        try:
            states = json.loads(proxy.call_finish(result).unpack()[0])
        except (GLib.Error, ValueError):
            return
        for path, state in zip(paths, states):
            if self.states.get(path) != state:
                self.states[path] = state
                if path in self.files:
                    self.files[path].invalidate_extension_info()
        if self.unknown:
            self.flush()

    def flush(self):
        """Ask about newly shown items in one batch."""
        if not self.busy and self.proxy.get_name_owner():
            self.busy = True
            paths, self.unknown = list(self.unknown), set()
            self.ask(paths)
        return False

    def mapping_for(self, path):
        if not path or any(part.startswith(".peedrive") for part in path.split(os.sep)):
            return None
        for item in self.mappings:
            root = item["mapping"]["local"]
            if path == root or path.startswith(root + os.sep):
                return item
        return None

    def update_file_info(self, file):
        path = file.get_location().get_path()
        if not self.mapping_for(path):
            return
        self.files.pop(path, None)
        self.files[path] = file
        while len(self.files) > REMEMBERED:
            del self.files[next(iter(self.files))]
        if path in self.states:
            if self.states[path] in EMBLEMS:
                file.add_emblem(EMBLEMS[self.states[path]])
        elif not self.unknown:
            self.unknown.add(path)
            GLib.timeout_add(100, self.flush)
        else:
            self.unknown.add(path)

    def get_file_items(self, files):
        if not files or not self.mapping_for(files[0].get_location().get_path()):
            return []
        items = []
        if len(files) == 1:
            path = files[0].get_location().get_path()
            mapping = self.mapping_for(path)["mapping"]
            relative = os.path.relpath(path, mapping["local"])
            state = self.states.get(path)
            kind = "folder" if files[0].is_directory() else "file"
            if relative != "." and state == "none":
                item = Nautilus.MenuItem(name="Peedrive::select", label=f"Sync This {kind.title()}",
                                         tip=f"Keep this {kind} in sync with Proton Drive")
                item.connect("activate", lambda *_: self.select("add", relative))
                items.append(item)
            elif relative != "." and state not in (None, "ignored"):
                item = Nautilus.MenuItem(name="Peedrive::deselect", label="Stop Syncing",
                                         tip=f"Keep this {kind} in Proton Drive only; the local copy moves to Recovered files")
                item.connect("activate", lambda *_: self.select("remove", relative))
                items.append(item)
        item = Nautilus.MenuItem(name="Peedrive::open", label="Open Peedrive", tip="View sync status")
        item.connect("activate", lambda *_: subprocess.Popen(["peedrive-app"]))
        items.append(item)
        return items

    def select(self, change, relative):
        request = json.dumps({"action": "select-drive", change: relative})
        self.call("Submit", GLib.Variant("(s)", (request,)), lambda proxy, result, _: self.submitted(proxy, result))

    def submitted(self, proxy, result):
        try:
            proxy.call_finish(result)
        except GLib.Error:
            pass
        self.refresh()
