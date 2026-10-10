#!/usr/bin/python3
"""Collect dependency metadata and available license texts for the binary package."""
import json
import os
from pathlib import Path
import re
import shutil
import subprocess

project=Path(__file__).resolve().parent.parent
output=project/'dist/dependency-notices'
output.mkdir(parents=True,exist_ok=True)
items=[]

def collect(name,version,license_name,directory):
    safe=re.sub(r'[^a-zA-Z0-9_.-]','_',f'{name}-{version}')
    target=output/safe
    target.mkdir(exist_ok=True)
    texts=[]
    for file in directory.iterdir():
        if file.is_file() and file.name.upper().startswith(('LICENSE','LICENCE','COPYING','NOTICE','COPYRIGHT')):
            shutil.copyfile(file,target/file.name)
            texts.append(file.name)
    items.append({'name':name,'version':version,'license':license_name,'texts':texts})

env={**os.environ,'PATH':str(Path.home()/'.cargo/bin')+os.pathsep+os.environ['PATH']}
metadata=json.loads(subprocess.check_output(['cargo','metadata','--format-version=1','--locked','--all-features'],cwd=project,env=env,text=True))
for package in metadata['packages']:
    collect(package['name'],package['version'],package['license'],Path(package['manifest_path']).parent)
for directory,subdirs,files in os.walk(project/'node_modules'):
    if 'package.json' not in files:continue
    try:
        package=json.loads((Path(directory)/'package.json').read_text())
        if 'name' in package and 'version' in package:collect(package['name'],package['version'],package.get('license'),Path(directory))
    except (ValueError,OSError):continue
(output/'manifest.json').write_text(json.dumps(items,indent=2)+'\n')
print(f'Collected metadata and available license texts for {len(items)} dependencies')
