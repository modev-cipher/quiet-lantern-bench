# A test exe links Tauri's dialogs but carries no app manifest, so comctl32 v5 loads and it dies
# with STATUS_ENTRYPOINT_NOT_FOUND. The shipped app embeds its manifest; tests get a side-by-side one.
run_tests() {
  local selector=("$1"); shift
  [ "${selector[0]}" = --test ] && { selector+=("$1"); shift; }
  [ "${1:-}" = -- ] && shift
  local exe
  exe=$(cargo test "${selector[@]}" --no-run --message-format=json | python -c "import json,sys;[print(m['executable']) for m in map(json.loads,sys.stdin) if m.get('executable') and m.get('profile',{}).get('test')]" | tail -n 1)
  [ -n "$exe" ] || return 1
  cat > "$exe.manifest" <<'XML'
<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0"><dependency><dependentAssembly>
<assemblyIdentity type="win32" name="Microsoft.Windows.Common-Controls" version="6.0.0.0" processorArchitecture="*" publicKeyToken="6595b64144ccf1df" language="*"/>
</dependentAssembly></dependency></assembly>
XML
  "$exe" "$@"
}
