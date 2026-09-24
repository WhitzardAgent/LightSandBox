"""Deliberate failing fixture for T00-C02: the runner must propagate exit 17."""
import sys

if __name__ == '__main__':
    print('exit17 fixture: exiting with code 17 on purpose')
    sys.exit(17)
