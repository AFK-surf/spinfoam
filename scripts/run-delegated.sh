#!/bin/sh
# Run inside an exclusively delegated cgroup v2 subtree. No privilege escalation.
set -eu
if [ "$#" -lt 2 ]; then
    echo 'usage: run-delegated.sh CGROUP_ROOT SPINFOAM [ARGS...]' >&2
    exit 2
fi
spinfoam_cgroup_root=$(readlink -f "$1")
shift
if [ ! -f "$spinfoam_cgroup_root/cgroup.controllers" ]; then
    echo 'expected a delegated cgroup v2 subtree' >&2
    exit 2
fi
mkdir -p "$spinfoam_cgroup_root/runtime"
printf '%s\n' "$$" > "$spinfoam_cgroup_root/runtime/cgroup.procs"
printf '+cpu +memory +pids\n' > "$spinfoam_cgroup_root/cgroup.subtree_control"
exec "$@" --compiler-cgroup "$spinfoam_cgroup_root"
