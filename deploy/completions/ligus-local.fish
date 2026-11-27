# fish completion for deploy/local/ligus-local. Install: copy to
# ~/.config/fish/completions/ligus-local.fish. deploy/tests/agent-deploy-test.sh
# keeps it in step with the script's commands and flags.

set -l cmds preflight install-cli configure start unit verify status stop uninstall upgrade help
complete -c ligus-local -f
complete -c ligus-local -n "not __fish_seen_subcommand_from $cmds" -a preflight -d 'check this machine; changes nothing'
complete -c ligus-local -n "not __fish_seen_subcommand_from $cmds" -a install-cli -d 'install the ligus CLI'
complete -c ligus-local -n "not __fish_seen_subcommand_from $cmds" -a configure -d 'write the env file and the data dir'
complete -c ligus-local -n "not __fish_seen_subcommand_from $cmds" -a start -d 'start serve or worker'
complete -c ligus-local -n "not __fish_seen_subcommand_from $cmds" -a unit -d 'print the unit start would install'
complete -c ligus-local -n "not __fish_seen_subcommand_from $cmds" -a verify -d 'process, health and sandbox checks'
complete -c ligus-local -n "not __fish_seen_subcommand_from $cmds" -a status -d 'the same as verify'
complete -c ligus-local -n "not __fish_seen_subcommand_from $cmds" -a stop -d 'stop it'
complete -c ligus-local -n "not __fish_seen_subcommand_from $cmds" -a uninstall -d 'stop and remove it'
complete -c ligus-local -n "not __fish_seen_subcommand_from $cmds" -a upgrade -d 'upgrade the CLI, restart and verify'
complete -c ligus-local -n "not __fish_seen_subcommand_from $cmds" -a help -d 'help for a command, or exit-codes'
complete -c ligus-local -n '__fish_seen_subcommand_from help' -a 'preflight install-cli configure start unit verify stop uninstall upgrade exit-codes'

# Common flags.
complete -c ligus-local -l role -x -a 'serve worker' -d 'which process'
complete -c ligus-local -l data-dir -r -a '(__fish_complete_directories)' -d "this process's own data dir"
complete -c ligus-local -l env-file -r -F -d 'the env file configure writes'
complete -c ligus-local -l json -d 'one JSON object on stdout'
complete -c ligus-local -s o -l output -x -a 'json text' -d 'output format'
complete -c ligus-local -s q -l quiet -d 'only warnings and failures on stderr'
complete -c ligus-local -l color -x -a 'auto always never' -d 'colour FAIL and WARN on stderr'
complete -c ligus-local -s h -l help -d "this command's help"

# Command flags.
complete -c ligus-local -n '__fish_seen_subcommand_from preflight configure' -l listen -x -d 'listen address'
complete -c ligus-local -n '__fish_seen_subcommand_from install-cli upgrade' -l version -x -d 'a release, vX.Y.Z'
complete -c ligus-local -n '__fish_seen_subcommand_from install-cli upgrade' -l from-source -d 'build from this checkout'
complete -c ligus-local -n '__fish_seen_subcommand_from install-cli upgrade' -l bin-dir -r -a '(__fish_complete_directories)' -d 'where to install'
complete -c ligus-local -n '__fish_seen_subcommand_from configure' -l label -x -d 'the box label'
complete -c ligus-local -n '__fish_seen_subcommand_from configure' -l relay-url -x -d 'the relay URL'
complete -c ligus-local -n '__fish_seen_subcommand_from configure' -l secret-backend -x -a 'file keychain secret-service' -d 'secret store'
complete -c ligus-local -n '__fish_seen_subcommand_from configure' -l throwaway-workspace -d 'a random throwaway workspace'
complete -c ligus-local -n '__fish_seen_subcommand_from configure' -l force -d 'rewrite an env file'
complete -c ligus-local -n '__fish_seen_subcommand_from start stop verify status unit upgrade' -l launchd -d 'a LaunchAgent'
complete -c ligus-local -n '__fish_seen_subcommand_from start stop verify status upgrade' -l systemd -d 'a systemd user unit'
complete -c ligus-local -n '__fish_seen_subcommand_from uninstall' -l delete-data -d 'also delete the data dir'
