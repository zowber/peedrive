#!/usr/bin/python3
"""Exercise the real daemon and CLI in an isolated D-Bus session and directory."""
import json
import os
from pathlib import Path
import subprocess
import tempfile
import time

project=Path(__file__).resolve().parent.parent
binary=project/'target/debug/peedrive'

def load_nautilus_extension():
    """The real extension, with a stand-in for the Nautilus bindings that GNOME Files provides."""
    import gi, importlib.util, sys, types
    from gi.repository import GObject
    real=gi.require_version
    gi.require_version=lambda namespace,version: None if namespace=='Nautilus' else real(namespace,version)
    class MenuItem(GObject.Object):
        __gsignals__={'activate':(GObject.SignalFlags.RUN_FIRST,None,())}
        def __init__(self,name,label,tip):
            super().__init__();self.label=label
    nautilus=types.ModuleType('gi.repository.Nautilus')
    nautilus.InfoProvider=type('InfoProvider',(),{});nautilus.MenuProvider=type('MenuProvider',(),{});nautilus.MenuItem=MenuItem
    sys.modules['gi.repository.Nautilus']=nautilus
    spec=importlib.util.spec_from_file_location('peedrive_nautilus',project/'packaging/peedrive-nautilus.py')
    module=importlib.util.module_from_spec(spec);spec.loader.exec_module(module)
    return module.PeedriveExtension()

class FakeFile:
    def __init__(self,path):self.path=str(path);self.emblems=[];self.invalidated=0
    def get_location(self):
        from gi.repository import Gio
        return Gio.File.new_for_path(self.path)
    def is_directory(self):return os.path.isdir(self.path)
    def add_emblem(self,emblem):self.emblems.append(emblem)
    def invalidate_extension_info(self):self.invalidated+=1

def until(condition,seconds=15):
    from gi.repository import GLib
    deadline=time.monotonic()+seconds
    while not condition():
        assert time.monotonic()<deadline,'Timed out'
        GLib.MainContext.default().iteration(False);time.sleep(.01)

def emblem(extension,file):
    """Show the file and return the emblem GNOME Files would draw once the state arrives."""
    file.emblems.clear();extension.update_file_info(file)
    until(lambda:file.path in extension.states)
    file.emblems.clear();extension.update_file_info(file)
    return file.emblems

with tempfile.TemporaryDirectory(prefix='peedrive-service-test-') as temporary:
    base=Path(temporary)
    local=base/'local';local.mkdir()
    (local/'from-local.txt').write_text('local original')
    data=base/'state'
    env={**os.environ,'PEEDRIVE_DATA_DIR':str(data),'PEEDRIVE_ADAPTER':str(project/'tests/mock-adapter.py')}
    daemon=subprocess.Popen([binary,'daemon'],env=env,stdout=subprocess.DEVNULL,stderr=subprocess.PIPE,text=True)
    def cli(*args,fail=False):
        result=subprocess.run([binary,*args],env=env,capture_output=True,text=True,timeout=30)
        if fail:
            assert result.returncode != 0, result.stdout
            return result.stderr
        assert result.returncode==0,result.stderr
        return json.loads(result.stdout)
    try:
        deadline=time.monotonic()+15
        while True:
            try:
                if cli('status')['signed_in']:break
            except AssertionError:pass
            assert time.monotonic()<deadline,'Daemon did not become ready'
            time.sleep(.1)
        activity=cli('status')['activity']
        assert any('Connected to Proton Drive' in a['message'] for a in activity)
        sync_folder=base/'Peedrive'/'{account email}'/'Proton Drive'
        cli('setup',str(sync_folder))
        assert cli('status')['sync_folder']==str(sync_folder)
        remote=data/'fake-drive' 
        (remote/'from-drive.txt').write_text('remote original')
        added=cli('add',str(local),'root');mid=added['mapping']['id']
        assert added['result']=={'synced':True,'conflicts':0},'A first sync without deletions should not need review'
        assert (remote/'from-local.txt').read_text()=='local original'
        assert (local/'from-drive.txt').read_text()=='remote original'
        cli('add',str(local),'root',fail=True)
        cli('pause',mid)
        (local/'from-local.txt').write_text('paused edit')
        cli('sync',mid,fail=True)
        assert (remote/'from-local.txt').read_text()=='local original'
        cli('resume',mid)
        cli('preview',mid)
        (local/'from-local.txt').write_text('edit after preview')
        cli('approve',mid,fail=True)
        cli('preview',mid);cli('approve',mid)
        assert (remote/'from-local.txt').read_text()=='edit after preview'
        (local/'from-local.txt').write_text('local conflict')
        (remote/'from-local.txt').write_text('remote conflict')
        cli('sync',mid)
        assert (local/'from-local.txt').read_text()=='remote conflict'
        assert any(p.read_text()=='local conflict' for p in local.glob('*conflict*'))
        (data/'offline').touch()
        (local/'from-local.txt').write_text('edit while offline')
        cli('sync',mid,fail=True)
        state=next(m for m in cli('status')['mappings'] if m['mapping']['id']==mid)
        assert state['state']=='Offline',state
        assert state['mapping']['approved'] and not state['mapping']['paused'],'A network failure required review'
        (data/'offline').unlink()
        cli('sync',mid)
        assert (remote/'from-local.txt').read_text()=='edit while offline'
        versions=cli('versions',mid,'from-local.txt')
        assert versions[0]['active'] and len(versions)>=3,versions
        cli('restore-version',mid,'from-local.txt',versions[-1]['uid'])
        assert (local/'from-local.txt').read_text()=='local original','The oldest version should download'
        assert any(r['original']=='from-local.txt' and Path(r['stored']).read_text()=='edit while offline' for r in cli('status')['recovered'])
        cli('restore-version',mid,'from-local.txt',cli('versions',mid,'from-local.txt')[1]['uid'])
        assert (local/'from-local.txt').read_text()=='edit while offline'
        (local/'from-local.txt').rename(local/'renamed.txt');cli('sync',mid)
        assert (remote/'renamed.txt').read_text()=='edit while offline' and not (remote/'from-local.txt').exists()
        assert cli('status')['activity'][0]['message']=='Renamed from-local.txt to renamed.txt'
        (local/'renamed.txt').rename(local/'from-local.txt');cli('sync',mid)
        assert cli('set','bandwidth_limit','500000')['bandwidth_limit']==500000
        (local/'limited.txt').write_text('limited');cli('sync',mid)
        assert (data/'last-limit').read_text()=='500000','Uploads should carry the speed limit'
        assert cli('set','bandwidth_limit','null')['bandwidth_limit'] is None
        (local/'limited.txt').unlink();cli('sync',mid)
        cli('ignore',mid,'from-drive.txt')
        (local/'from-drive.txt').unlink();cli('sync',mid)
        assert (remote/'from-drive.txt').exists(),'Ignored file became a deletion'
        cli('remove',mid)
        assert local.exists() and (remote/'from-local.txt').exists()
        assert cli('status')['mappings']==[]
        selected=cli('select-drive','from-drive.txt'); sid=selected['mapping']['id']
        expected=base/'Peedrive'/'test@example.com'/'Proton Drive'
        assert selected['mapping']['local']==str(expected)
        assert selected['mapping']['selection']==['from-drive.txt']
        assert (expected/'from-drive.txt').exists(),'Selecting files should download them without review'
        assert not (expected/'from-local.txt').exists()
        extension=load_nautilus_extension()
        extension.refresh();until(lambda:extension.mappings)
        assert emblem(extension,FakeFile(expected/'from-drive.txt'))==['emblem-default']
        (expected/'New folder').mkdir();(expected/'New folder'/'notes.txt').write_text('local notes')
        folder=FakeFile(expected/'New folder')
        assert emblem(extension,folder)==[],'An unselected folder has no emblem'
        [sync]=[i for i in extension.get_file_items([folder]) if i.label!='Open Peedrive']
        assert sync.label=='Sync This Folder';sync.emit('activate')
        until(lambda:(remote/'New folder'/'notes.txt').exists())
        until(lambda:extension.states.get(folder.path)=='synced')
        [stop]=[i for i in extension.get_file_items([folder]) if i.label!='Open Peedrive']
        assert stop.label=='Stop Syncing';stop.emit('activate')
        until(lambda:not (expected/'New folder').exists())
        assert (remote/'New folder'/'notes.txt').exists(),'Stop syncing must keep the Drive copy'
        assert (expected/'from-drive.txt').exists(),'Stop syncing one item keeps the rest'
        cli('select-drive')
        assert not (expected/'from-drive.txt').exists(),'Unticked files should leave this computer'
        assert (remote/'from-drive.txt').exists(),'Unticking must not delete from Drive'
        recovered=[r for r in cli('status')['recovered'] if r['original']=='from-drive.txt']
        assert len(recovered)==1 and Path(recovered[0]['stored']).exists()
        cli('restore',str(recovered[0]['id']))
        assert (expected/'from-drive.txt').exists() and not Path(recovered[0]['stored']).exists()
        # New local items outside the selection upload; a same-named unselected Drive item does not merge.
        (expected/'Made here').mkdir(); (expected/'Made here'/'note.txt').write_text('new locally')
        (remote/'Drive only').mkdir(); (remote/'Drive only'/'big.bin').write_text('remote content')
        (expected/'Drive only').mkdir(); (expected/'Drive only'/'local.txt').write_text('local twin')
        cli('sync',sid)
        assert (remote/'Made here'/'note.txt').read_text()=='new locally','A new local folder was not uploaded'
        assert 'Made here' in cli('status')['mappings'][0]['mapping']['selection']
        assert not (remote/'Drive only'/'local.txt').exists() and not (expected/'Drive only'/'big.bin').exists()
        assert any('Drive only is not syncing' in e['message'] for e in cli('status')['activity'])
        cli('remove',sid)
        live=cli('live-test')
        assert live['passed'] and live['mapping_paused']
        assert (remote/'from-local.txt').read_text()=='edit while offline'
        try: cli('quit')
        except (AssertionError, json.JSONDecodeError): pass  # The service may exit before replying.
        assert daemon.wait(timeout=15)==0,'Quit did not stop the service cleanly'
        daemon=subprocess.Popen([binary,'daemon'],env=env,stdout=subprocess.DEVNULL,stderr=subprocess.PIPE,text=True)
        deadline=time.monotonic()+15
        while True:
            try:
                cli('status');break
            except AssertionError:pass
            assert time.monotonic()<deadline,'Daemon did not restart'
            time.sleep(.1)
        daemon.terminate()
        assert daemon.wait(timeout=15)==0,'Daemon did not stop cleanly on SIGTERM'
        print('PASS: daemon, D-Bus, CLI, first-sync review, overlap rejection, pause/resume, stale approval, conflicts, offline retry, renames, versions, speed limit, ignores, GNOME Files emblems and menu, new local items, disconnect and quit and clean shutdown')
    finally:
        daemon.terminate()
        try: daemon.wait(timeout=5)
        except subprocess.TimeoutExpired:daemon.kill();daemon.wait()
