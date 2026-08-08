function pp
    # If arguments are passed, forward them directly to the Rust binary
    if test (count $argv) -gt 0
        command pp $argv
        return
    end

    # Default behaviour: run Rust binary (interactive skim picker) and cd to chosen directory
    set -l repo (command pp)
    if test -n "$repo"
        cd $repo
    end
end
