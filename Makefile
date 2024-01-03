render-diagrams:
	@(scripts/render-diagrams.sh '' 'design/v1' 'design/v1/assets')
	@(scripts/render-diagrams.sh '' 'design/v2' 'design/v2/assets')
	@(echo "\n"'⚠️ Adobe XD diagrams are not exported automatically.'"\n"'  Open `*.xd` files and batch export diagrams in SVG format in <./assets/>.')
release:
	@(scripts/release.sh)
format:
	@(bash .githooks/pre-commit)
