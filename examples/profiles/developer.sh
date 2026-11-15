nsh profile inherit vanilla
nsh set prompt.template '{cwd}{git} {symbol} '
nsh set prompt.symbol 'λ'
nsh set prompt.symbol.success '❯'
nsh set prompt.symbol.error '⚡'
nsh set prompt.path_color '#8DBEFF'
nsh set prompt.git_color '#98D293'
nsh set prompt.symbol_color '#DFCFA4'
nsh module enable git-prompt
nsh module enable highlighting
nsh set history.max_size 10000
nsh set history.ignore_duplicates true
