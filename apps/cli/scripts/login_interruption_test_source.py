"""Read the production fixture source for offline contract assertions."""
import ast
from pathlib import Path


FILES = (
    'dev-login-interruption-smoke.py',
    'login_interruption_account.py',
    'login_interruption_control.py',
    'login_interruption_continuations.py',
    'login_interruption_cleanup.py',
    'login_interruption_transfer.py',
)
SOURCE = '\n'.join(Path(__file__).with_name(name).read_text(encoding='utf-8') for name in FILES)
TREE = ast.parse(SOURCE)
