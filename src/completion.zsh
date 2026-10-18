    setopt localoptions extendedglob
    local -a values=() dirs=() options=()
    local completion value
    for completion in "${completions[@]}"; do
        [[ -n $completion ]] || continue
        # Clap 用未转义的冒号分隔说明；\: 和 \\ 属于候选值。
        value="${(M)completion##([^:\\]|\\?)#}"
        # --root=PATH/ 也是目录，必须先于选项识别并保留可继续输入的 /。
        if [[ $value == */ ]]; then
            dirs+=("${value%/}${completion:${#value}}")
        elif [[ $value == -* ]]; then
            options+=("$completion")
        else
            values+=("$completion")
        fi
    done

    local ret=1
    if (( $#values )); then
        _describe -V 'values' values -V twig-values && ret=0
    fi
    if (( $#dirs )); then
        _describe -V 'directories' dirs -V twig-values -S '/' -r '/' && ret=0
    fi
    if (( $#options )); then
        _describe -V 'options' options -V twig-options && ret=0
    fi
    return ret
