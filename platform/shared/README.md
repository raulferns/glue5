# Glue 5 app icon

`Glue5.icon` is the shared Icon Composer source for iOS, iPadOS and the
macOS app bundle. Open this package with Icon Composer bundled with Xcode 26.

The two 1024 × 1024 PNG layers live inside the package:

- `Assets/01-background.png`: opaque dark sunset city; material effects off.
- `Assets/02-five.png`: illustrated five with real alpha; separate foreground
  group with specular highlights and a subtle neutral shadow. Translucency and
  blur are off to preserve the ivory face and crisp ink contours.

Groups in `icon.json` are ordered front to back. The foreground scale is 0.82;
the imported transparent artwork has its own padding. The result matches the
approved compact icon proportions and leaves room for the system mask.
Dark appearance dims the background; tinted appearance suppresses it and uses
a white foreground fill. iOS and macOS share the square composition.

The iOS XcodeGen spec includes this package as a resource and sets the target
app icon name to `Glue5`. Xcode compiles Liquid Glass assets and generates
fallback icons for older systems (the deployment target remains iOS 16.4).
Do not add rounded corners to the source PNGs or copy the package as a plain
folder resource. `mise run macos:build` compiles it with `actool` and packages
both `Assets.car` and the fallback `Glue5.icns` in the Mac app.

Verification: Xcode 26.3 Release iPhoneOS build with signing disabled succeeded.
Standalone macOS asset compilation also produced `Assets.car` and `Glue5.icns`.
Dynamic motion and tinted rendering still need
checking on an actual device; static previews do not exercise those effects.

Artwork separation used built-in ImageGen edits of the approved dark sunset
icon: extract the five onto transparency, and remove the five/reconstruct the
hidden city for the background. ImageGen redrew these layers, so they are not
a lossless pixel extraction. PNGs were normalized to 1024 × 1024 with `sips`.
