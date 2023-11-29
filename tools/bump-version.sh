#!/usr/bin/env sh

RS="sdp-common sdp-device-id-service sdp-identity-service sdp-injector sdp-macros sdp-proc-macros sdp-test-macros"
HS="k8s/crd k8s/chart"

bump_version() {
    local f=$1
    local m=$2
    local c=$3
    local s=$4
    awk -F$5 -vM=$2 -vC=$c -vV=$4 -vD=2021 '
function vf(vo, m, s, vv) {
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
  print(s " \"" v "\"")
  V=vv
}
/^version/ && C==1 && V==0 {vf($2, M, "version =", 1)}
!/^version/ && !/^edition/ && C==1 {print($0)}
/^version/ && C==2 && V==1 {vf($2, M, "version:", 2)}
/^appVersion/ && C==2 && V==2 {vf($2, M, "appVersion:", 3)}
!/^version/ && !/^appVersion/ && C==2 {print($0)}
/^edition/ {print("edition ="  "\"" D "\"")}
' $f
}

# Bump cargo version
for r in $RS; do
    f=$r/Cargo.toml
    mv "$f" "$f.cp"
    bump_version "$f.cp" ${1:-Z} 1 0 = > "$f"
    rm "$f.cp"
done

# Bump chart version
for r in $HS; do
    f=$r/Chart.yaml
    mv "$f" "$f.cp"
    bump_version "$f.cp" ${1:-Z} 2 1 : > "$f"
    rm "$f.cp"
done
