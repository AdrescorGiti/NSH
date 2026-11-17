"""Pure classification and score rules shared by the runner and its tests."""


def classify(status, stdout, stderr, expected_status, expected_stdout, expected_stderr):
    if status < 0:
        return 'crash'
    if status != expected_status:
        return 'wrong_exit_status'
    if stdout != expected_stdout:
        return 'wrong_output'
    if expected_stderr is not None and stderr != expected_stderr:
        return 'unexpected_stderr'
    return 'pass'


def parser_limit(status, stdout, stderr):
    return status == 2 and stdout == b"" and stderr == b"nsh: shell syntax nesting limit exceeded\n"


def score(components, critical):
    weights = {'correctness': 3000, 'stability': 2500, 'memory': 1500,
               'performance': 1500, 'interactive': 1500}
    result = round(sum(weights[name] * max(0, min(1, components[name])) for name in weights))
    return min(result, 3000) if critical else result
