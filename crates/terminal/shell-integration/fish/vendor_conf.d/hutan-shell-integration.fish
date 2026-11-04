# Hutan shell integration for fish: OSC 133 prompt marks and OSC 7 working directory.
#
#   A  prompt start          B  prompt end / input start
#   C  command output start  D;<status>  command finished
#
# Hutan prepends its shell-integration directory to XDG_DATA_DIRS, so fish loads this file
# from fish/vendor_conf.d before the user's config.fish. It can also be sourced by hand:
#   source "$HUTAN_SHELL_INTEGRATION_DIR/fish/vendor_conf.d/hutan-shell-integration.fish"

# Hand XDG_DATA_DIRS back to the user's value, for this shell and everything it starts.
if set -q HUTAN_ORIG_XDG_DATA_DIRS
    set -gx XDG_DATA_DIRS $HUTAN_ORIG_XDG_DATA_DIRS
else if set -q HUTAN_XDG_DATA_DIRS_UNSET
    set -e XDG_DATA_DIRS
end
set -e HUTAN_ORIG_XDG_DATA_DIRS
set -e HUTAN_XDG_DATA_DIRS_UNSET

status is-interactive; or return 0
set -q __hutan_integration_loaded; and return 0
set -g __hutan_integration_loaded 1

# fish 4.0+ marks prompts itself (feature flag mark-prompt): A, C and D; the prompt-end mark B
# and an OSC 7 on every prompt since 4.4. Only what fish does not emit is added here.
set -g __hutan_native_marks 0
set -g __hutan_native_complete 0
if status test-feature mark-prompt
    set __hutan_native_marks 1
    if string match -qr -- '^(4\.([4-9]|[1-9][0-9]+)|([5-9]|[1-9][0-9]+))(\.|$|-)' $version
        set __hutan_native_complete 1
    end
end

function __hutan_osc7
    set -l path_encoded $PWD
    if string match -qr -- '[^A-Za-z0-9/._~-]' $PWD
        # Percent-encode byte by byte.
        set path_encoded (string escape --style=url -- $PWD)
    end
    printf '\e]7;file://%s%s\a' $hostname $path_encoded
end

function __hutan_wrap_prompt
    # Wrap fish_prompt (never replace it): the original keeps its name and output, B follows it.
    # Also re-wraps when the user redefines fish_prompt during the session.
    if functions fish_prompt | string match -q -- '*__hutan_orig_fish_prompt*'
        return
    end
    functions -c fish_prompt __hutan_orig_fish_prompt
    function fish_prompt
        __hutan_orig_fish_prompt $argv
        printf '\e]133;B\a'
    end
end

function __hutan_on_prompt --on-event fish_prompt
    if test $__hutan_native_complete -eq 0
        __hutan_osc7
    end
    if test $__hutan_native_marks -eq 0
        printf '\e]133;A\a'
    end
    if test $__hutan_native_complete -eq 0
        __hutan_wrap_prompt
    end
end

if test $__hutan_native_marks -eq 0
    function __hutan_on_preexec --on-event fish_preexec
        printf '\e]133;C\a'
    end

    function __hutan_on_postexec --on-event fish_postexec
        printf '\e]133;D;%s\a' $status
    end
end

# Agents' own OpenTelemetry (TEL-2, ADR 0026). While the daemon's loopback receiver listens, a
# Hutan terminal has HUTAN_OTLP_ENDPOINT and HUTAN_OTLP_TOKEN; `claude` and `codex` then become
# functions that ask `hutan telemetry env <agent>` what to put in front of that one launch (nothing
# unless the agent is enabled and nothing of yours, a managed policy's or the agent's own is in the
# way: it never overrides an OTEL_* variable or a setting) and run the program found on PATH with
# it. A `claude` or `codex` function of your own (in functions/ or defined before this file) is left
# alone; one that config.fish defines afterwards replaces ours.
if set -q HUTAN_OTLP_ENDPOINT; and test -n "$HUTAN_OTLP_ENDPOINT"; and set -q HUTAN_BIN
    function __hutan_otlp_launch
        set -l agent $argv[1]
        set -e argv[1]
        set -l launch_env
        set -l launch_args
        if test -x "$HUTAN_BIN"; and command -sq $agent
            for line in ("$HUTAN_BIN" telemetry env $agent 2>/dev/null)
                switch $line
                    case 'env *'
                        set launch_env $launch_env (string sub -s 5 -- $line)
                    case 'arg *'
                        set launch_args $launch_args (string sub -s 5 -- $line)
                end
            end
        end
        if set -q launch_env[1]
            command env $launch_env $agent $launch_args $argv
        else
            command $agent $argv
        end
    end
    for agent in claude codex
        if not functions -q $agent
            eval "function $agent --wraps=$agent --description 'Hutan: report $agent to its session'
                __hutan_otlp_launch $agent \$argv
            end"
        end
    end
    set -e agent
end
