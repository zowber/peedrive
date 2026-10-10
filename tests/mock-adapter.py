#!/usr/bin/python3
"""Local-only protocol fixture. Never contacts Proton or reads account credentials."""
import hashlib
import json
import os
from pathlib import Path
import shutil
import sys

root = Path(os.environ["PEEDRIVE_DATA_DIR"]) / "fake-drive"
root.mkdir(parents=True, exist_ok=True)

def node(path):
    if not path.exists():
        return None
    folder = path.is_dir()
    digest = None if folder else hashlib.sha1(path.read_bytes()).hexdigest()
    return {"kind": "folder" if folder else "file", "uid": str(path.relative_to(root)), "version": digest or "directory", "hash": digest, "size": 0 if folder else path.stat().st_size}

def target(p):
    relative = p["path"]
    assert not relative.startswith('/') and '..' not in relative.split('/')
    return base(p) / relative

def base(p):
    return root if p.get('root','root')=='root' else root/p['root']

def history(p):
    """Earlier contents of a file, oldest first, as Drive would keep them."""
    return root.parent/"revisions"/p["path"]

def keep(p):
    path=target(p)
    if path.is_file():
        folder=history(p);folder.mkdir(parents=True,exist_ok=True)
        (folder/f"{len(list(folder.iterdir())):04}").write_bytes(path.read_bytes())

class Offline(Exception):
    pass

def run(method, p):
    if method == "hello": return {"protocol": 1}
    if (root.parent / "offline").exists() and method not in ("auth.status", "browse"):
        raise Offline("getaddrinfo ETIMEOUT drive-api.proton.me")
    if method == "auth.status": return {"signed_in": True, "email": "test@example.com", "login_url": None, "error": None}
    if method == "browse": return {"uid": "root", "name": "Test Drive", "children": [], "ancestors": []}
    if method == "snapshot": return {str(path.relative_to(base(p))): node(path) for path in sorted(base(p).rglob('*'))}
    if method == "inspect": return node(target(p))
    if method in ("upload","download"):
        (root.parent/"last-limit").write_text(json.dumps(p.get("limit")))
    if method == "upload":
        path=target(p)
        assert node(path) == p["expected"]
        keep(p)
        shutil.copyfile(p["source"],path)
        return node(path)
    if method == "download":
        path=root/p["entry"]["uid"]
        assert node(path)["version"] == p["entry"]["version"]
        with open(p["destination"],"xb") as file: file.write(path.read_bytes())
        return {}
    if method == "mkdir":
        path=target(p);path.mkdir();return node(path)
    if method == "revisions":
        folder=history(p);older=sorted(folder.iterdir()) if folder.exists() else []
        current=target(p).read_bytes()
        versions=[{"uid":f"{p['path']}#{v.name}","time":v.stat().st_mtime_ns//10**9,"size":v.stat().st_size,"active":False} for v in older]
        return [{"uid":f"{p['path']}#current","time":0,"size":len(current),"active":True},*reversed(versions)]
    if method == "restoreRevision":
        name=p["revision"].rsplit("#",1)[1];content=(history(p)/name).read_bytes()
        keep(p);target(p).write_bytes(content);return {}
    if method == "move":
        source,destination=base(p)/p["from"],base(p)/p["to"]
        assert node(source)==p["expected"] and not destination.exists()
        source.rename(destination);return node(destination)
    if method == "trash":
        path=target(p);assert node(path)==p["expected"]
        path.rmdir() if path.is_dir() else path.unlink()
        return {}
    raise ValueError("Unknown fixture operation")

for line in sys.stdin:
    request=json.loads(line)
    try: output={"id":request["id"],"result":run(request["method"],request.get("params",{}))}
    except Offline as error: output={"id":request["id"],"error":{"message":str(error),"kind":"network","retryable":True}}
    except Exception as error: output={"id":request["id"],"error":{"message":str(error)}}
    print(json.dumps(output),flush=True)
