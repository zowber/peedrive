#!/usr/bin/python3
"""Synthetic D-Bus state and folder listings for native GTK rendering checks."""
import json
import os
import time
from gi.repository import Gio, GLib

scene = os.environ.get('PEEDRIVE_UI_SCENE', 'overview')
now = int(time.time())
def ago(seconds):
    return time.strftime('%Y-%m-%d %H:%M:%S', time.gmtime(now - seconds))

mapping = {
    'id': 'example', 'local': '/home/user/Proton Drive', 'remote': 'root',
    'remote_label': 'My files', 'root_device': 1, 'root_inode': 1,
    'paused': False, 'approved': True, 'ignores': ['*.tmp'], 'selection': ['Finance', 'Letters', 'Insurance policy.pdf'],
}
plan = {'operations': [
    {'path': f'Old projects/2019/file-{i}.pdf', 'action': 'trash_local', 'reason': 'Deleted remotely',
     'local': {'kind': 'file', 'version': 'v', 'uid': 'u', 'size': 1_200_000, 'hash': None}, 'remote': None} for i in range(14)],
    'deletions': 14, 'conflicts': 0}
mapping_status = {'mapping': mapping, 'state': 'Up to date', 'message': 'All changes synced', 'pending': 0,
                  'plan': None, 'done': 0, 'last_synced': now - 120, 'files': 128}
transfers = []
if scene == 'syncing':
    mapping_status.update(state='Syncing', pending=12, done=2)
    transfers = [{'id': 1, 'direction': 'upload', 'path': 'Finance/site-photos.zip', 'size': 12_000_000, 'bytes': 6_100_000},
                 {'id': 2, 'direction': 'download', 'path': 'Letters/Lease.pdf', 'size': 900_000, 'bytes': 200_000}]
if scene == 'issues':
    mapping_status.update(state='Review changes', plan=plan)

def event(i, seconds, kind, path, message):
    return {'id': i, 'time': ago(seconds), 'kind': kind, 'path': path, 'message': message}
activity = [
    event(9, 120, 'upload', 'Finance/Budget 2027.xlsx', 'Uploaded Finance/Budget 2027.xlsx'),
    event(8, 540, 'download', 'Letters/Meeting notes.md', 'Downloaded Letters/Meeting notes.md'),
    event(7, 3600, 'conflict', 'Letters/Floor plan v3.pdf', 'Kept both versions of Letters/Floor plan v3.pdf'),
    event(10, 60, 'moved', 'Archive/Old projects', 'Moved Projects to Archive/Old projects'),
    event(6, 3900, 'upload', 'Insurance policy.pdf', 'Uploaded Insurance policy.pdf'),
    event(5, 90000, 'removed_remote', 'Letters/old-draft.txt', 'Moved to Drive Trash Letters/old-draft.txt'),
    event(4, 93000, 'download', 'Finance/Tax return 2025.pdf', 'Downloaded Finance/Tax return 2025.pdf'),
    event(3, 96000, 'info', None, 'Connected to Proton Drive'),
]
state = {
    'signed_in': scene != 'setup', 'account_email': 'andy@example.com',
    'sync_folder': None if scene == 'setup' else '~/Proton Drive',
    'login_url': None, 'account_error': None, 'transfers': transfers, 'online': True, 'metered': False,
    'mappings': [] if scene == 'setup' else [mapping_status],
    'activity': activity,
    'recovered': [
        {'id': 1, 'mapping': 'example', 'original': 'Letters/old-draft.txt', 'stored': '/tmp/x/old-draft.txt',
         'reason': 'Removed from Proton Drive', 'time': ago(90000), 'size': 4100},
        {'id': 2, 'mapping': 'example', 'original': 'Letters/Meeting notes.md', 'stored': '/tmp/y/Meeting notes.md',
         'reason': 'Replaced by a newer Drive version', 'time': ago(540), 'size': 18000},
    ],
    'conflicts': [{'id': 1, 'mapping': 'example', 'path': 'Letters/Floor plan v3.pdf',
                   'copy': 'Letters/Floor plan v3 (conflicted copy 2026-10-10 1a2b).pdf', 'time': ago(3600)}],
    'settings': {'notifications': True, 'recovery_days': None, 'pause_on_metered': False},
    'quota': {'used': 4_200_000_000, 'max': 200_000_000_000},
}
children = [
    {'uid': 'finance', 'name': 'Finance', 'kind': 'folder', 'supported': True, 'size': 2_400_000_000},
    {'uid': 'letters', 'name': 'Letters', 'kind': 'folder', 'supported': True, 'size': 86_000_000},
    {'uid': 'scans', 'name': 'Scans 2025', 'kind': 'folder', 'supported': True, 'size': 1_100_000_000},
    {'uid': 'old', 'name': 'Old projects', 'kind': 'folder', 'supported': True, 'size': 14_800_000_000},
    {'uid': 'doc', 'name': 'Meeting notes', 'kind': 'file', 'supported': False, 'document': True, 'size': 0},
    {'uid': 'ins', 'name': 'Insurance policy.pdf', 'kind': 'file', 'supported': True, 'size': 1_300_000},
    {'uid': 'cv', 'name': 'CV draft.odt', 'kind': 'file', 'supported': True, 'size': 48_000},
]
xml = '''<node><interface name="io.github.zowber.Peedrive.Sync">
<method name="Status"><arg type="s" direction="out"/></method>
<method name="Submit"><arg type="s" direction="in"/><arg type="s" direction="out"/></method>
<method name="Job"><arg type="s" direction="in"/><arg type="s" direction="out"/></method>
</interface></node>'''
jobs = {}
def call(c, s, p, i, method, args, inv):
    if method == 'Status':
        result = state
    elif method == 'Submit':
        request = json.loads(args.unpack()[0]); key = str(len(jobs))
        if request['action'] == 'browse':
            uid = request.get('uid') or 'root'
            jobs[key] = {'result': {'uid': uid, 'name': 'My files', 'ancestors': [], 'children': children if uid == 'root' else []}}
        elif request['action'] == 'search':
            jobs[key] = {'result': []}
        else:
            jobs[key] = {'error': 'This is a read-only UI fixture'}
        inv.return_value(GLib.Variant('(s)', (key,))); return
    else:
        result = jobs.get(args.unpack()[0], {})
    inv.return_value(GLib.Variant('(s)', (json.dumps(result),)))
def acquired(connection, _name):
    info = Gio.DBusNodeInfo.new_for_xml(xml).interfaces[0]
    connection.register_object('/io/github/zowber/Peedrive', info, call, None, None)
    print('ready', flush=True)
Gio.bus_own_name(Gio.BusType.SESSION, 'io.github.zowber.Peedrive.Sync', Gio.BusNameOwnerFlags.NONE, acquired, None, None)
GLib.MainLoop().run()
