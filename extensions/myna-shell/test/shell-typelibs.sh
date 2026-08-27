# shell-typelibs.sh - locate mutter's and gnome-shell's private typelibs.
#
# Sourced, not executed:
#
#     . "$HERE/shell-typelibs.sh"        # sets GI_TYPELIB_PATH/LD_LIBRARY_PATH
#
# mutter and gnome-shell install Clutter, Cogl, Meta and St beside their own
# libraries rather than on the default search path, so anything that imports
# them from plain `gjs` has to find them first. Two probes need this
# (gpu-probe.sh, compat-probe.sh) and a third would too; the globbing is
# fiddly enough that a second copy of it would drift.
#
# Returns 1 when there is nothing to find - callers turn that into their own
# "cannot judge" exit 77, since only they know what that means.

shell_typelibs_export() {
    # Newest first, so a machine with several installed probes the one a
    # Shell started here would actually load.
    # An unmatched glob stays literal, so each candidate is filtered by -d.
    local dirs
    dirs=$(printf '%s\n' /usr/lib/*/mutter-* /usr/lib/mutter-* \
                          /usr/lib/*/gnome-shell /usr/lib/gnome-shell |
           while read -r dir; do
               [ -d "$dir" ] && printf '%s\n' "$dir"
           done | sort -rV | tr '\n' ':')
    [ -n "$dirs" ] || return 1
    export GI_TYPELIB_PATH="${dirs}${GI_TYPELIB_PATH:-}"
    export LD_LIBRARY_PATH="${dirs}${LD_LIBRARY_PATH:-}"
}
