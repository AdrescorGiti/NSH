"""Deterministic, POSIX-defined workloads and existing integration suites."""


def cases(profile):
    result = [
        dict(name='quotes', category='parser', script='printf "<%s>\\n" "a b" \'c d\' "$(printf x)" "$((1 + 2))"\n', stdout='<a b>\n<c d>\n<x>\n<3>\n', stderr=''),
        dict(name='heredoc', category='parser', script="cat <<'END'\n$x $(false)\nEND\n", stdout='$x $(false)\n', stderr=''),
        dict(name='unexpected-eof', category='parser', script='if true; then\n', status=2, stdout='', stderr=None),
        dict(name='redirect-and-substitute', category='posix', script="printf '%s\\n' 'a b' > result; value=$(cat result); printf '<%s>\\n' \"$value\"; cat result\n", stdout='<a b>\na b\n', stderr=''),
        dict(name='pipeline', category='execution', script="printf '%s\\n' one two | cat | wc -l\n", stdout='2\n', stderr=''),
        dict(name='recovery', category='recovery', script="false; printf 'recovered\\n'; (exit 3); printf 'again\\n'\n", stdout='recovered\nagain\n', stderr=''),
        dict(name='memory-small', category='memory', script='i=0; while [ "$i" -lt 1000 ]; do x=abcdefghij; i=$((i+1)); done; printf "done\\n"\n', stdout='done\n', stderr='', timeout=15),
    ]
    # Existing suites are reused to avoid inventing Lua APIs or job-control expectations.
    for name, category, timeout in [('lua_plugins', 'lua', 25), ('jobs_pty', 'signals', 30),
                                    ('differential', 'posix', 45), ('startup_pty', 'recovery', 30)]:
        argv = ['/usr/local/bin/python3', '/opt/nsh/tests/' + name + '.py', '/opt/nsh/target/release/nsh']
        if name == 'differential':
            argv += ['--dash', '/usr/local/bin/hardtest-dash', '--dash-version', '0.5.13.5',
                     '--bash', '/bin/bash', '--yash', '/usr/local/bin/hardtest-yash']
        result.append(dict(name=name, category=category, argv=argv, timeout=timeout, suite=True))
    sizes = {'QUICK': [100], 'STRESS': [100, 1000, 10000],
             'EXTREME': [100, 1000, 10000, 100000, 1000000]}[profile]
    for count in sizes:
        result.append(dict(name=f'commands-{count}', category='execution', load=count,
                           script=':\n' * count + "printf 'ok\\n'\n", stdout='ok\n', stderr='', timeout=20))
    if profile != 'QUICK':
        result.extend([
            dict(name='functions-loops', category='execution', script='f() { x=$((x+1)); }; x=0; while [ "$x" -lt 2000 ]; do f; done; printf "%s\\n" "$x"\n', stdout='2000\n', stderr='', timeout=20),
            dict(name='external-processes', category='execution', script='/usr/bin/true\n' * 200 + "printf 'ok\\n'\n", stdout='ok\n', stderr='', timeout=20),
            dict(name='sigterm-pty', category='signals', argv=['/usr/local/bin/python3', '/opt/nsh/hardtest/signal_stress.py'], suite=True, timeout=15),
            dict(name='lua-budgets', category='lua', argv=['/usr/local/bin/python3', '/opt/nsh/hardtest/lua_stress.py'], suite=True, timeout=25),
            dict(name='lua-lifecycle', category='lua', argv=['/usr/local/bin/python3', '/opt/nsh/tests/plugins.py', '/opt/nsh/target/release/nsh'], suite=True, timeout=30),
            dict(name='memory-repetition', category='memory', script='i=0; while [ "$i" -lt 8000 ]; do x=abcdefghij; i=$((i+1)); done; printf "done\\n"\n', stdout='done\n', stderr='', timeout=20),
        ])
    if profile == 'EXTREME':
        for depth in (10, 100, 500):
            result.append(dict(name=f'nesting-{depth}', category='parser', load=depth,
                               script='( ' * depth + "printf 'ok\\n'" + ' )' * depth + '\n',
                               stdout='ok\n', stderr='', timeout=20, parser_limit=True))
        result.append(dict(name='large-token-list', category='parser', load=10000,
                           script='set -- ' + 'x ' * 10000 + '\nprintf "%s\\n" "$#"\n', stdout='10000\n', stderr='', timeout=20))
    return result
