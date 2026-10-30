#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 Slavi Pantaleev
#
# SPDX-License-Identifier: Apache-2.0
#
# End-to-end test of the image: a signed push through the receiver,
# then the executor run the way its systemd unit runs it (privileged, host PID namespace),
# deploying a throwaway repository with git as the current user and the hooks as root,
# then a run whose container is removed by force mid-deploy (as on the unit's timeout), which must kill every hook process.
# The executor acts on THIS host; everything it touches lives in a temporary directory.
# Needs docker, git, curl, openssl and python3; the current user must be in /etc/passwd.
set -euo pipefail

image=${1:-kukaroll:dev}
work=$(mktemp -d)
receiver="kukaroll-e2e-receiver-$$"
executor="kukaroll-e2e-executor-$$"
hc_port=$((20000 + RANDOM % 10000))
receiver_port=$((30000 + RANDOM % 10000))
cleanup() {
	docker rm --force "$receiver" "$executor" >/dev/null 2>&1 || true
	# A hook process the timed-out run should have taken along (root's, so killed from the host's side).
	[[ -s ${work:-}/background.pid ]] && docker run --rm --privileged --pid=host --user 0:0 "$image" \
		host-exec root kill -KILL "$(cat "$work/background.pid")" >/dev/null 2>&1 || true
	[[ -n ${hc_pid:-} ]] && kill "$hc_pid" 2>/dev/null || true
	rm -rf "$work"
}
trap cleanup EXIT
fail() { echo "FAIL: $*" >&2; exit 1; }
git_() { git -c user.name=e2e -c user.email=e2e@example.com -c commit.gpgsign=false "$@"; }

cd "$work"
mkdir -m 0777 spool
mkdir secrets
echo s3cret > secrets/shop
chmod 0644 secrets/shop
git_ init --quiet --bare --initial-branch=main origin
git_ clone --quiet origin work 2>/dev/null
echo 1 > work/version
git_ -C work add version
git_ -C work commit --quiet -m 1
git_ -C work push --quiet origin main
git_ clone --quiet origin checkout
echo 'SECRET=kept' > checkout/.env

cat > receiver.yaml <<YAML
schema_version: 1
listen: 0.0.0.0:8080
spool_dir: /spool
projects:
  shop:
    branch: main
    webhook: {provider: forgejo, secret_file: /secrets/shop}
YAML
cat > executor.yaml <<YAML
schema_version: 1
projects:
  shop:
    checkout: $work/checkout
    remote: origin
    branch: main
    git_user: $(id -un)
    hooks_user: root
    lock_file: $work/deploy.lock
    pre_flight: ["sh", "-c", "echo \"pre:\$(cat version):\$KUKAROLL_COMMIT\" >> $work/hooks.log"]
    deploy: ["sh", "-c", "echo \"deploy:\$(cat version):\$(id -un):\$HOME:\$(git rev-parse HEAD)\" >> $work/hooks.log"]
    healthchecks: {ping_url: "http://127.0.0.1:$hc_port/ping/e2e"}
YAML
docker run --rm --user "$(id -u):$(id -g)" --volume "$work:$work:ro" "$image" check-config receiver "$work/receiver.yaml"
docker run --rm --user "$(id -u):$(id -g)" --volume "$work:$work:ro" "$image" check-config executor "$work/executor.yaml"

python3 -I -c '
import http.server, sys
class Handler(http.server.BaseHTTPRequestHandler):
    def do_POST(self):
        self.rfile.read(int(self.headers.get("content-length", 0)))
        print(self.path, flush=True)
        self.send_response(200)
        self.end_headers()
    def log_message(self, *args):
        pass
http.server.HTTPServer(("127.0.0.1", int(sys.argv[1])), Handler).serve_forever()
' "$hc_port" > pings.log &
hc_pid=$!

echo "== receiver"
docker run --detach --name "$receiver" --user 65534:65534 --read-only --cap-drop=ALL \
	--publish "127.0.0.1:$receiver_port:8080" \
	--volume "$work/receiver.yaml:/receiver.yaml:ro" --volume "$work/secrets:/secrets:ro" \
	--volume "$work/spool:/spool" "$image" receive --config /receiver.yaml >/dev/null
for _ in $(seq 50); do curl --silent --fail "http://127.0.0.1:$receiver_port/health" >/dev/null && break; sleep 0.1; done
[[ -e spool/shop ]] || fail "catch-up did not create the spool file"
rm spool/shop

echo 2 > work/version
git_ -C work commit --quiet -am 2
git_ -C work push --quiet origin main
tip=$(git -C work rev-parse HEAD)
body='{"ref":"refs/heads/main","deleted":false}'
signature=$(printf %s "$body" | openssl dgst -sha256 -hmac s3cret -r | cut -d' ' -f1)
status=$(curl --silent --output /dev/null --write-out '%{http_code}' -H 'X-Forgejo-Event: push' \
	-H "X-Forgejo-Signature: $signature" --data "$body" "http://127.0.0.1:$receiver_port/hooks/shop")
[[ $status == 202 ]] || fail "push got HTTP $status"
[[ -e spool/shop ]] || fail "push did not create the spool file"
status=$(curl --silent --output /dev/null --write-out '%{http_code}' -H 'X-Forgejo-Event: push' \
	-H 'X-Forgejo-Signature: 00' --data "$body" "http://127.0.0.1:$receiver_port/hooks/shop")
[[ $status == 401 ]] || fail "unsigned push got HTTP $status"

echo "== executor"
execute() {
	docker run --rm --privileged --pid=host --user 0:0 --log-driver=none \
		--volume "$work/executor.yaml:/executor.yaml:ro" "$image" execute --config /executor.yaml shop
}
# kukaroll-execute@.service deletes the spool file before starting the executor.
rm spool/shop
execute
[[ $(git -C checkout rev-parse HEAD) == "$tip" ]] || fail "checkout is not at the tip"
[[ $(git -C checkout rev-parse refs/kukaroll/deployed) == "$tip" ]] || fail "deployed commit not recorded"
[[ -z $(find checkout ! -user "$(id -u)") ]] || fail "files not owned by the git user: $(find checkout ! -user "$(id -u)")"
[[ $(cat checkout/.env) == SECRET=kept ]] || fail "untracked file was touched"
expected="pre:1:$tip
deploy:2:root:/root:$tip"
[[ $(cat hooks.log) == "$expected" ]] || fail "hooks ran unexpectedly: $(cat hooks.log)"
execute
[[ $(wc -l < hooks.log) == 2 ]] || fail "an already deployed commit was deployed again"
sleep 0.5
expected_pings="/ping/e2e/start
/ping/e2e
/ping/e2e/start
/ping/e2e"
[[ $(cat pings.log) == "$expected_pings" ]] || fail "unexpected pings: $(cat pings.log)"

echo "== timed-out run"
# On timeout, kukaroll-execute@.service's ExecStopPost removes the container by force.
# That must kill every hook process, including one that left the hook's process group (they stay in the container's cgroup).
cat > executor-timeout.yaml <<YAML
schema_version: 1
projects:
  shop:
    checkout: $work/checkout
    remote: origin
    branch: main
    git_user: $(id -un)
    hooks_user: root
    lock_file: $work/deploy.lock
    pre_flight: null
    deploy: ["sh", "-c", "setsid sleep 3600 & echo \$! > $work/background.pid; sleep 3600"]
    healthchecks: null
YAML
echo 3 > work/version
git_ -C work commit --quiet -am 3
git_ -C work push --quiet origin main
docker run --detach --rm --name "$executor" --privileged --pid=host --user 0:0 --log-driver=none \
	--volume "$work/executor-timeout.yaml:/executor.yaml:ro" "$image" execute --config /executor.yaml shop >/dev/null
for _ in $(seq 300); do [[ -s background.pid ]] && break; sleep 0.1; done
[[ -s background.pid ]] || fail "the deploy hook did not start its background process"
background=$(cat background.pid)
# Gone, or a zombie (dead, waiting for whoever it was reparented to).
gone() { [[ ! -e /proc/$background ]] || [[ $(cut -d' ' -f3 "/proc/$background/stat" 2>/dev/null) == Z ]]; }
gone && fail "the background process ended by itself"
docker rm --force "$executor" >/dev/null
for _ in $(seq 100); do gone && break; sleep 0.1; done
gone || fail "a hook process (pid $background) survived the forced removal of the executor's container"
rm background.pid

echo "PASS"
