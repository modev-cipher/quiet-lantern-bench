import json, os, sys, urllib.request
catalog, dest = sys.argv[1:3]
data = json.load(open(catalog))
items = data if isinstance(data, list) else data.get("models", data)
model = next(m for m in items if m.get("id") == "kokoro-82m")
for f in model["download"]["files"]:
    path = os.path.join(dest, f["dest_path"])
    os.makedirs(os.path.dirname(path), exist_ok=True)
    urllib.request.urlretrieve(f["url"], path)
    print(f["dest_path"], os.path.getsize(path))
