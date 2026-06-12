{
  description = "Timeline plugin: documents — server binary + web bundle.";

  inputs = {
    # In real use point this at the published timeline repo, e.g.
    #   timeline.url = "github:proxtx/timeline";
    # For local development against a checkout, override on the CLI:
    #   nix build --override-input timeline path:/abs/path/to/timeline
    timeline.url = "github:proxtx/timeline";
    flake-utils.follows = "timeline/flake-utils";
  };

  outputs = { self, timeline, flake-utils }:
    flake-utils.lib.eachDefaultSystem (system: {
      packages.default = timeline.lib.buildPlugin {
        inherit system;
        name = "timeline_plugin_documents";
        src = ./.;
      };
      packages.timeline_plugin_documents = self.packages.${system}.default;
    });
}
