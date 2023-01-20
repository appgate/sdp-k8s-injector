#!/usr/bin/env sh

SS="sdp-common sdp-device-id-service sdp-identity-service sdp-injector sdp-macros sdp-proc-macros sdp-test-macros"

bump_version() {
    local f=$1
    local m=$2
    awk -F= -vM=$2 '
function vf(vo, m) {
  gsub(/[" ]/, "", vo)
  split(vo, vs, ".")
  switch (m) {
    case "Z":
      v = vs[1] "." vs[2] "." vs[3] + 1
      break
    case "Y":
      v = vs[1] "." vs[2] + 1 ".0"
      break
    case "X":
      v = vs[1] + 1 ".0.0"
      break
    default:
      print("Wrong parameter " m " [X|Y|Z]")
      exit(1)
  }
  print("version = \"" v "\"")
  V=1
}
/^version/ && V==0 {vf($2, M)}
!/^version/ {print($0)}
' $f
}

for s in $SS; do
    f=$s/Cargo.toml
    mv "$f" "$f.cp"
    bump_version "$f.cp" ${1:-Z} > "$f"
    rm "$f.cp"
done
