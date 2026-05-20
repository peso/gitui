/*! The `graph_cache` module implements an adaptor of the gleisbau library
to the internals of gitui. */

use std::cell::RefCell;
use std::ops::Range;
use std::rc::Rc;

use gleisbau::backend::git2::Builder;
use gleisbau::backend::git2::TrackMap;
use gleisbau::layout::layout_track_range;
use gleisbau::layout::TrackLayout;
use gleisbau::print::format::CommitFormat;
use gleisbau::print::unicode::print_graph_terminal;
use gleisbau::print::unicode::GraphLines;
use gleisbau::settings::BranchOrder;
use gleisbau::settings::BranchSettings;
use gleisbau::settings::BranchSettingsDef;
use gleisbau::settings::Characters;
use gleisbau::settings::MergePatterns;
use gleisbau::settings::Settings;
use ratatui::text::Line;

use asyncgit::sync::CommitInfo;
use asyncgit::sync::RepoPath;

/*
/// Commit index type. Index into [CommitList.commits]
type Cinx = u32;

/// Branch index type
type Binx = u32;
*/

/** Main struct used for branch graph.
*/
pub struct GraphCache {
	/// gleisbau configuration. This is used by the builder
	/// as well as when updating layout
	settings: Rc<Settings>,

	/// Topology.
	topo: Rc<RefCell<TrackMap>>,

	/// Geometry
	geo: Option<TrackLayout>,

	/// Document
	doc: Option<GraphLines>,

	/// Builder used to incrementally fill topology data from repository.
	/// Discarded when all commits has been processed.
	builder: Option<Builder>,
}

fn extract_settings(_repo_path: RefCell<RepoPath>) -> Settings {
	// TODO read gleisbau config file in repository folder
	// or remove _repo_path argument

	Settings {
		// Reverse the order of commits
		reverse_commit_order: false,
		// Debug printing and drawing
		debug: false,
		// Compact text-based graph
		compact: false,
		// Colored text-based graph
		colored: false,
		// Include remote branches?
		include_remote: false,
		// Formatting for commits
		format: CommitFormat::OneLine, // TODO eliminate - not needed
		// Text wrapping options
		wrapping: None, // TODO eliminate - not needed
		// Characters to use for text-based graph
		characters: Characters::round(),
		// Branch column sorting algorithm
		branch_order: BranchOrder::ShortestFirst(true),
		// Settings for branches
		branches: BranchSettings::from(BranchSettingsDef::none())
			.expect("Default settings should never fail"),
		// Regex patterns for finding branch names in merge commit summaries
		merge_patterns: MergePatterns::default(),
	}
}

impl GraphCache {
	pub fn new(repo_path: RefCell<RepoPath>) -> Self {
		let settings = Rc::new(extract_settings(repo_path));
		let topo = Rc::new(RefCell::new(TrackMap::new()));
		let builder = Some(Builder::new(topo.clone()));
		Self {
			settings,
			topo,
			geo: None,
			doc: None,
			builder,
		}
	}

	/// Expand the branch topology
	pub fn add_commits(&mut self, commits: &Vec<CommitInfo>) {
		let builder = self
			.builder
			.as_mut()
			// Initial version expects builder to live forever
			// Later version may create and discard as needed
			.expect("Builder is never discarded");
		for c in commits {
			let id = c.id.into();
			let message = c.message.clone();
			let parents: Vec<_> =
				c.parents.iter().map(|&id| id.into()).collect();

			builder.add_commit(id, message, parents);
		}
	}

	/// Layout a section of commits
	pub fn compute_layout(&mut self, commit_range: Range<usize>) {
		let track_layout = layout_track_range(
			&self.topo.borrow(),
			commit_range,
			&self.settings,
		)
		.expect("Valid Trackmap and range");

		// All commits are given 1 row for text
		let text_height = vec![1; track_layout.commit_count()];

		let graph_lines = print_graph_terminal(
			&self.settings,
			&self.topo.borrow(),
			&track_layout,
			&text_height,
		);

		self.geo = Some(track_layout);
		self.doc = Some(graph_lines);
	}

	/// Get a graph from the specified offset row in layout
	pub fn get_graph_line(&self, row: usize) -> Line<'static> {
		let line_ref = self
			.doc
			.as_ref()
			.and_then(|graph_lines| graph_lines.graph_lines.get(row));

		let string_to_line = |line: &String| Line::from(line.clone());
		let default_line = || Line::from("%% no graph data %%");
		line_ref.map_or_else(default_line, string_to_line)
	}
}
