import zipfile,json,hashlib,pathlib,sys
p=pathlib.Path(sys.argv[1])
with zipfile.ZipFile(p) as z:
    names=z.namelist();assert len(names)==3 and len(set(names))==3
    m=json.loads(z.read('manifest.json'));c=z.read('config.json');assert len(c)<=1048576 and isinstance(json.loads(c),dict)
    assert m['id']=='dev.codey.antigravity-router' and m['version']=='0.9.0'
    assert m['abiVersion']==1 and m['platform']=='windows' and m['arch']=='x86_64'
    assert set(m['capabilities'])=={'provider.route.v1','request.lifecycle.v1'}
    assert m['entry']=='lib/codey_plugin_antigravity_router.dll'
    assert set(names)=={'manifest.json','config.json',m['entry']}
    assert hashlib.sha256(z.read(m['entry'])).hexdigest()==m['librarySha256']
    if len(sys.argv)>2:
        pathlib.Path(sys.argv[2]).write_bytes(z.read(m['entry']))
print('PASS native package: exact three files / ABI / platform / capabilities / config / DLL SHA256')
