# Shared by the tvOS tasks (sourced; not a task itself: not executable): the
# iOS tasks' helpers (devices, launch) for an Apple TV, with its library,
# project and SDK.
platform=tvOS
. "$(dirname "${BASH_SOURCE[0]}")/../ios/lib.sh"

# Builds the app's library (the glue5 crate as a static library) for the
# Apple TV, or the target given (its simulator's), and generates the Xcode
# project from platform/tvos/project.yml, next to it. The deployment target
# is as in project.yml (only in this build's environment, as for iOS).
tvos_project() {
    TVOS_DEPLOYMENT_TARGET=16.4 cargo rustc --release -p glue5 --lib \
        --crate-type staticlib --target "${1:-aarch64-apple-tvos}"
    xcodegen generate --quiet --spec platform/tvos/project.yml
}

# xcodebuild DESTINATION [SETTING...]; the app ends up in $app (the
# simulator's in $sim_app).
app="$PWD/target/tvos/derived/Build/Products/Release-appletvos/Glue 5.app"
sim_app="$PWD/target/tvos/derived/Build/Products/Release-appletvsimulator/Glue 5.app"
xcodebuild_app() {
    local destination=$1 sdk=appletvos
    shift
    case $destination in *Simulator*) sdk=appletvsimulator ;; esac
    xcodebuild -project platform/tvos/Glue5.xcodeproj -scheme Glue5 -configuration Release \
        -sdk "$sdk" -destination "$destination" -quiet \
        -derivedDataPath target/tvos/derived "$@" build
}
