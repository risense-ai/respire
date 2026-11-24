"""Fixed failure codes for synthetic Runtime smoke checks."""



class Failure(Exception):
    pass


def require(condition, code):
    if not condition:
        raise Failure(code)


def client_only_diagnostic(result, before, after, profile_exists, endpoint_exists):
    # Fixed predicates only: never persist captured output, filenames or hashes.
    return {
        'case_id': 'client_only_no_profile_writes',
        'actual_exit': result.returncode if type(result.returncode) is int else None,
        'failed_checks': [name for name, passed in (
            ('command_rejected', type(result.returncode) is int and result.returncode != 0),
            ('home_files_unchanged', before == after),
            ('profile_absent', not profile_exists),
            ('endpoint_absent', not endpoint_exists),
        ) if not passed],
    }


DEV = 'https://api-dev.rsrs.rs'
DASH = 'https://dash.rsrs.rs'
