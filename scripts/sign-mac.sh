#!/bin/zsh
# Signs a dev build of the tray app with a fixed identifier and this Mac's Apple Development
# certificate, if it has one: scripts/sign-mac.sh target/debug/crossglide
#
# "Start at login" runs the build directly, with no Terminal around it, so macOS asks for
# Accessibility permission for crossglide itself. It remembers that permission by the signature.
# The linker's ad-hoc signature changes with every build, which makes macOS forget it; a
# certificate signature doesn't. CROSSGLIDE_SIGN_IDENTITY picks another certificate (its SHA-1
# or name). Never fails, so a launch doesn't either.

identity=${CROSSGLIDE_SIGN_IDENTITY:-$(security find-identity -v -p codesigning 2>/dev/null \
    | awk '/"(Apple Development|Developer ID Application): / { print $2; exit }')}
if [ -z "$identity" ]; then
    echo "sign: no code-signing certificate, so macOS forgets Accessibility for crossglide after rebuilds"
    exit 0
fi
codesign -f -s "$identity" -i com.crossglide.tray "$1" 2>/dev/null \
    || echo "sign: codesign failed, so macOS may forget Accessibility for crossglide"
exit 0
