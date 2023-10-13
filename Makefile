render-diagrams:
	@(scripts/render-diagrams.sh '' 'design')
	@(echo "\n"'⚠️ Adobe XD diagrams are not exported automatically.'"\n"'  Open `*.xd` files and batch export diagrams in SVG format in <./assets/>.')
