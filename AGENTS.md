# Contributor constraints

- Keep ShowMesh authoritative for show state. This application observes locally and applies bounded local policy.
- Never add kill, stop, or forced-restart controls to process supervision.
- Never infer that Resolume or another consumer received LTC merely because this app decoded it.
- Advertise only capabilities whose modules are enabled, completely configured, and operationally probed.
- Preserve unknown, disconnected, unavailable, failed, disabled, and unobserved as distinct states.
- User configuration belongs in the application UI; do not require hand-edited configuration files.
- Preserve macOS 11 and Windows 10 as deployment floors until an explicit compatibility decision changes them.
- Do not claim platform support from compilation alone; packaged runtime evidence is required.
