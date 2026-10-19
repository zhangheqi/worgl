#!/bin/sh
# Makes the state of a three-member playground cluster: one mint, copied to
# every member before any of them starts, and the mint's authority key for
# the faucet. Runs once, as root, before anything else in compose.yaml; does
# nothing if it ran before.
set -eu

if [ -e /members/1/cluster.json ]; then
    echo "already set up: playground mint $(cat /faucet/mint)"
    exit 0
fi

work=$(mktemp -d)
state="$work/state"
worgl-server init --state "$state" >/dev/null
worgl-server create-mint --state "$state" \
    --layers "${WORGL_LAYERS:-4}" \
    --layer-size "${WORGL_LAYER_SIZE:-4194304}" \
    --rotate-every "${WORGL_ROTATE_EVERY:-28800}" >"$work/created"
mint=$(sed -n 's/^mint  *\([0-9a-f]\{64\}\)$/\1/p' "$work/created")

# The members never read the authority key; only the faucet holds it.
mv "$state/mints/$mint/mint.key" /faucet/mint.key
echo "$mint" >/faucet/mint

set -- \
    --member 1=member1:8080,member1:7080 \
    --member 2=member2:8080,member2:7080 \
    --member 3=member3:8080,member3:7080
worgl-server join-cluster --state "$state" --node 1 "$@" >/dev/null
for node in 1 2 3; do
    cp -a "$state/." "/members/$node/"
    worgl-server join-cluster --state "/members/$node" --node "$node" "$@" >/dev/null
done

chown -R 10001:10001 /members /faucet
rm -rf "$work"
echo "playground mint $mint"
