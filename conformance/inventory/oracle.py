"""Oracle (positive proof) status vocabulary.

Three states, mirroring docs/acceptance/01_PARITY_CONTRACT.md and T02:

* ORACLE_AVAILABLE        -- asset exists AND was executed successfully in the
                             reference environment. T02 enumeration never runs
                             the reference environment, so real enumeration
                             output must not contain this state by itself.
* ORACLE_NOT_YET_VERIFIED -- a gold patch / solution / other oracle asset is
                             present, but nobody has shown it still passes.
* ORACLE_UNAVAILABLE      -- the task ships no oracle asset at all.

Only ORACLE_AVAILABLE entries may join a positive-pass denominator.
"""

ORACLE_AVAILABLE = 'ORACLE_AVAILABLE'
ORACLE_NOT_YET_VERIFIED = 'ORACLE_NOT_YET_VERIFIED'
ORACLE_UNAVAILABLE = 'ORACLE_UNAVAILABLE'

ALL = (ORACLE_AVAILABLE, ORACLE_NOT_YET_VERIFIED, ORACLE_UNAVAILABLE)


def summarize_oracle(entries):
    """Aggregate oracle status across manifest entries.

    Positive-pass counts may only be built on ORACLE_AVAILABLE entries; the
    summary keeps the three states visibly separate to make that law
    mechanical to check.
    """
    summary = {state: 0 for state in ALL}
    total = 0
    for entry in entries:
        status = entry.get('oracle_status')
        if status not in summary:
            raise ValueError(f'unknown oracle_status {status!r} in {entry.get("uid")}')
        summary[status] += 1
        total += 1
    summary['total'] = total
    summary['positive_oracle_eligible'] = summary[ORACLE_AVAILABLE]
    return summary
