# Udon node definitions

Machine-generated snapshots of the Udon node definitions (extern signatures,
types, graph nodes) exposed by the VRChat Worlds SDK, one JSON file per Unity
editor version. This is the whitelist the M# code generator compiles against.

## Provenance

Each file is produced inside a Unity project with the VRChat Worlds SDK
installed, with the MenSharp package's **MenSharp > Export Udon Whitelist…**
(`unity/io.tesca.mensharp/Editor/MenSharpUdonNodes.cs`). The dump is a plain
enumeration of `UdonEditorManager.Instance.GetNodeDefinitions()` — factual
API metadata (type / method / parameter names and directions). No SDK code,
binaries, or documentation are included, and none may be committed to this
repository. Only live SDKs are dumped here: a beta or closed-beta SDK's
whitelist is read by the package in the user's own project
(`Library/MenSharp/udon-nodes/`) and handed to the compiler with
`--udon-nodes`, never committed.

| File | SDK source | Dumped |
|------|------------|--------|
| `2022.3.22f1.json` | VRChat Worlds SDK (VPM, 2026-08) | 2026-08-30 |
