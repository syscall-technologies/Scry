# Backlog

Ideas not yet built, roughly in priority order.

## Hardware H.264 decode (VideoToolbox, macOS)
Implement IronRDP's `H264Decoder` trait against a `VTDecompressionSession` (feed Annex-B
NAL units / SPS-PPS, convert NV12 → RGBA) so Fast/H.264 mode decodes on the Mac's media
engine instead of OpenH264 in software.
- **Benefit:** lower CPU/heat and better battery in H.264 mode (measured 18.5% CPU with
  OpenH264 vs 12.5% for Windows App; VideoToolbox would likely match or beat it). Memory
  likely drops too (no OpenH264 buffers).
- **Not a benefit:** won't make it visibly faster/smoother — decode isn't the bottleneck
  (network round-trip to the gateway dominates; decode is a few ms, small % busy).
- **Scope:** macOS-only, moderate; needs runtime verification on-device. Still AVC420.

## Auto-reconnect on a dropped session
Reconnect automatically when the link drops, ideally without a fresh MFA (try RDP
auto-reconnect cookie first). Today the viewer closes with the reason.

## Security hardening
- Pin the Cloud PC's TLS certificate (compare against `redirectedServerCert` from the ARM
  gateway reply) instead of accepting any cert on the inner TLS.
- Add a token/auth to the local MCP (:7879) and control (:7878) ports; today any local
  process can use them while a session is open.

## AVC444 for H.264
IronRDP only supports AVC420; Windows App uses AVC444 for crisper colored text in H.264.
Larger protocol work.

## Smaller polish
- Persist the launcher's graphics toggle (remember Sharp/Fast across launches).
- Flash the MCP "Copied ✓" briefly instead of leaving it set.
- Optionally make the viewer/sign-in windows accessory (no extra Dock tiles).
- Pointer lock / "capture system shortcuts" mode (needs macOS Accessibility permission).
- Clipboard: works where the tenant allows it; some tenants disable it via policy (⌘⇧V typing fallback covers
  local → Cloud PC).

## Platforms
- Linux/Windows runtime verification (CI builds all three; GUI launcher is egui, no GTK).
