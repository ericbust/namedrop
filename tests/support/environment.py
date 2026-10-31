from pathlib import Path


def isolated_home(home):
    home = Path(home)
    assert home.is_absolute(), 'fixture home must be absolute'
    return {
        'HC_SOCKET': '',
        'HOME': str(home),
        'XDG_STATE_HOME': str(home / '.local/state'),
        'XDG_CONFIG_HOME': str(home / '.config'),
        'XDG_DATA_HOME': str(home / '.local/share'),
        'XDG_CACHE_HOME': str(home / '.cache'),
        'XDG_RUNTIME_DIR': str(home / '.runtime'),
    }
