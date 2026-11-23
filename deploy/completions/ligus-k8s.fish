# fish completion for deploy/kubernetes/ligus-k8s. Install: copy to
# ~/.config/fish/completions/ligus-k8s.fish. deploy/tests/agent-deploy-test.sh
# keeps it in step with the script's commands and flags.

set -l cmds preflight secrets install render verify teardown uninstall check-rendered help
complete -c ligus-k8s -f
complete -c ligus-k8s -n "not __fish_seen_subcommand_from $cmds" -a preflight -d 'check the cluster; changes nothing'
complete -c ligus-k8s -n "not __fish_seen_subcommand_from $cmds" -a secrets -d 'create a Secret from files'
complete -c ligus-k8s -n "not __fish_seen_subcommand_from $cmds" -a install -d 'render, check, dry-run, helm upgrade --install'
complete -c ligus-k8s -n "not __fish_seen_subcommand_from $cmds" -a render -d 'install --dry-run'
complete -c ligus-k8s -n "not __fish_seen_subcommand_from $cmds" -a verify -d 'pods, sandbox, health, helm test'
complete -c ligus-k8s -n "not __fish_seen_subcommand_from $cmds" -a teardown -d 'helm uninstall (alias: uninstall)'
complete -c ligus-k8s -n "not __fish_seen_subcommand_from $cmds" -a uninstall -d 'the same as teardown'
complete -c ligus-k8s -n "not __fish_seen_subcommand_from $cmds" -a check-rendered -d 'check a rendered manifest offline'
complete -c ligus-k8s -n "not __fish_seen_subcommand_from $cmds" -a help -d 'help for a command, or exit-codes'
complete -c ligus-k8s -n '__fish_seen_subcommand_from help' -a 'preflight secrets install verify teardown check-rendered exit-codes'
complete -c ligus-k8s -n '__fish_seen_subcommand_from secrets' -a 'workspace generic'

# Common flags.
complete -c ligus-k8s -l context -x -a '(kubectl config get-contexts -o name 2>/dev/null)' -d 'kube context (required)'
complete -c ligus-k8s -s n -l namespace -x -d 'one namespace per workspace'
complete -c ligus-k8s -l release -x -d 'the Helm release'
complete -c ligus-k8s -l json -d 'one JSON object on stdout'
complete -c ligus-k8s -s o -l output -x -a 'json text' -d 'output format'
complete -c ligus-k8s -s q -l quiet -d 'only warnings and failures on stderr'
complete -c ligus-k8s -l color -x -a 'auto always never' -d 'colour FAIL and WARN on stderr'
complete -c ligus-k8s -s h -l help -d "this command's help"

# Command flags.
set -l img 'preflight install render'
complete -c ligus-k8s -n "__fish_seen_subcommand_from $img" -l image -x -d 'the Ligus image repo:tag'
complete -c ligus-k8s -n "__fish_seen_subcommand_from $img" -l image-repository -x -d 'image repository'
complete -c ligus-k8s -n "__fish_seen_subcommand_from $img" -l image-tag -x -d 'image tag'
complete -c ligus-k8s -n "__fish_seen_subcommand_from $img" -l image-digest -x -d 'image digest'
complete -c ligus-k8s -n '__fish_seen_subcommand_from preflight install render' -l storage-class -x -d 'StorageClass'
complete -c ligus-k8s -n '__fish_seen_subcommand_from preflight' -l probe -d 'measure in a temporary namespace'
complete -c ligus-k8s -n '__fish_seen_subcommand_from preflight' -l probe-image -x -d 'image for the probe pods'
complete -c ligus-k8s -n '__fish_seen_subcommand_from secrets' -l throwaway -d 'a random throwaway workspace'
complete -c ligus-k8s -n '__fish_seen_subcommand_from secrets' -l from-dir -r -a '(__fish_complete_directories)' -d 'files, one per key'
complete -c ligus-k8s -n '__fish_seen_subcommand_from secrets' -l name -x -d 'the Secret name'
complete -c ligus-k8s -n '__fish_seen_subcommand_from secrets' -l replace -d 'update an existing Secret'
complete -c ligus-k8s -n '__fish_seen_subcommand_from secrets install render' -l workspace-secret -x -d 'the workspace Secret'
complete -c ligus-k8s -n '__fish_seen_subcommand_from install render' -l scenario -x -a 'trial team-in-cluster-relay team-external-relay' -d 'the scenario'
complete -c ligus-k8s -n '__fish_seen_subcommand_from install render' -l dry-run -d 'render and server dry-run only'
complete -c ligus-k8s -n '__fish_seen_subcommand_from install render' -l relay-chart -r -a '(__fish_complete_directories)' -d 'a relay chart directory'
complete -c ligus-k8s -n '__fish_seen_subcommand_from install render' -l fetch-relay-chart -d 'fetch the pinned relay chart'
complete -c ligus-k8s -n '__fish_seen_subcommand_from install render' -l relay-image-repository -x -d 'relay image'
complete -c ligus-k8s -n '__fish_seen_subcommand_from install render' -l relay-image-tag -x -d 'relay image tag'
complete -c ligus-k8s -n '__fish_seen_subcommand_from install render' -l relay-url -x -d 'an https relay outside the cluster'
complete -c ligus-k8s -n '__fish_seen_subcommand_from install render' -l relay-namespace -x -d 'the in-cluster relay namespace'
complete -c ligus-k8s -n '__fish_seen_subcommand_from install render' -l relay-access-token -d "use the Secret's relay-access-token"
complete -c ligus-k8s -n '__fish_seen_subcommand_from install render' -l workers -x -d 'worker hosts'
complete -c ligus-k8s -n '__fish_seen_subcommand_from install render' -l no-serve -d 'no serve'
complete -c ligus-k8s -n '__fish_seen_subcommand_from install render' -l access-mode -x -a 'ReadWriteOncePod ReadWriteOnce' -d 'PVC access mode'
complete -c ligus-k8s -n '__fish_seen_subcommand_from install render' -l values -r -F -d 'extra values file'
complete -c ligus-k8s -n '__fish_seen_subcommand_from install render' -l timeout -x -d 'wait for readiness'
complete -c ligus-k8s -n '__fish_seen_subcommand_from verify' -l skip-helm-test -d 'skip helm test'
complete -c ligus-k8s -n '__fish_seen_subcommand_from teardown uninstall' -l delete-data -d 'also delete the PVCs'
complete -c ligus-k8s -n '__fish_seen_subcommand_from teardown uninstall' -l delete-secrets -d 'also delete the Secrets'
complete -c ligus-k8s -n '__fish_seen_subcommand_from teardown uninstall' -l delete-namespace -d 'also delete the namespace'
complete -c ligus-k8s -n '__fish_seen_subcommand_from check-rendered' -F -d 'a rendered manifest'
