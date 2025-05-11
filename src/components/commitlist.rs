use super::utils::logitems::{ItemBatch, LogEntry};
use crate::{
	app::Environment,
	components::{
		utils::string_width_align, CommandBlocking, CommandInfo,
		Component, DrawableComponent, EventState, ScrollType,
	},
	keys::{key_match, SharedKeyConfig},
	queue::{InternalEvent, Queue},
	strings::{self, symbol},
	try_or_popup,
	ui::style::{SharedTheme, Theme},
	ui::{calc_scroll_top, draw_scrollbar, Orientation},
};
use ansi_to_tui::IntoText;
use anyhow::Result;
use asyncgit::sync::{
	self, checkout_commit, BranchDetails, BranchInfo, CommitId,
	RepoPathRef, Tags,
};
use chrono::{DateTime, Local};
use crossterm::event::Event;
use git_graph::{
	graph::GitGraph,
	graph::Repository as Graph_Repository, // A reexport of git2::Repository
	print::unicode::print_unicode,
	settings::Settings as Graph_Settings,
	settings::BranchSettingsDef,
};
//use git2::{Repository as Graph_Repository};
use indexmap::IndexSet;
use itertools::Itertools;
use ratatui::{
	layout::{Alignment, Rect},
	style::Style,
	text::{Line, Span},
	widgets::{Block, Borders, Paragraph},
	Frame,
};
use std::{
	ops::Deref,
	borrow::Cow, 
	cell::{Cell, RefCell, Ref},
	cmp, collections::BTreeMap, rc::Rc,
	time::Instant,
};

/// Number of Spans to allocate per line
const ELEMENTS_PER_LINE: usize = 9;
/// Commits to fetch at a time
const SLICE_SIZE: usize = 1200;
/// Feature toggle
const LOG_GRAPH_ENABLED: bool = true;
const GRAPH_COLUMN_ENABLED: bool = true;

///
pub struct CommitList {
	repo: RepoPathRef,
	title: Box<str>,

	 /// Index into self.commits of user cursor
	selection: usize,

	highlighted_selection: Option<usize>,

	items: ItemBatch,

	/// The cached subset of commits in the graph
	graph_cache: RefCell<GitGraphCache>,

	/// Highlighted commits
	highlights: Option<Rc<IndexSet<CommitId>>>,
	commits: IndexSet<CommitId>,
	/// Commits that are marked. The .0 is used to provide a sort order.
	/// It contains an index into self.items.items
	marked: Vec<(usize, CommitId)>,
	scroll_state: (Instant, f32),
	tags: Option<Tags>,
	local_branches: BTreeMap<CommitId, Vec<BranchInfo>>,
	remote_branches: BTreeMap<CommitId, Vec<BranchInfo>>,
	current_size: Cell<Option<(u16, u16)>>,
	scroll_top: Cell<usize>,
	/// Number of commits on current page
	scroll_page: RefCell<usize>,
	theme: SharedTheme,
	queue: Queue,
	key_config: SharedKeyConfig,
}

#[derive(Default)]
struct GitGraphCache {
	/// Configuration for the graph
	settings: Option<Graph_Settings>,
	/// Skipped commits for graph cache
	skip: usize,
	/// The cached subset of commits in the graph
	gitgraph: Option<GitGraph>,
}

impl GitGraphCache {

}

fn default_graph_settings() -> Graph_Settings {
	use git_graph::print::format::CommitFormat;
	use git_graph::settings::{BranchOrder, BranchSettings, Characters, MergePatterns};

	let reverse_commit_order = false;
	let debug = false;
	let colored = true;
	let compact = true;
	let include_remote = false;
	let format = CommitFormat::OneLine;
	let wrapping = None;
	let style = Characters::round();

	Graph_Settings {
		reverse_commit_order,
		debug,
		colored,
		compact,
		include_remote,
		format,
		wrapping,
		characters: style,
		branch_order: BranchOrder::ShortestFirst(true),
		branches: BranchSettings::from(
			BranchSettingsDef::git_flow()
		).expect("Could not use default branch settings git_flow"),
		merge_patterns: MergePatterns::default(),
	}
}

impl CommitList {
	///
	pub fn new(env: &Environment, title: &str) -> Self {
		/*
		// Open the asyncgit repository as a git2 repository for GitGraph
		let repo_binding = env.repo.borrow();
		let repo_path = repo_binding.gitpath();
        let git2_repo = match Graph_Repository::open(repo_path) {
            Ok(repo) => repo,
            Err(e) => panic!("Unable to open git2 repository: {}", e), // Handle this error properly in a real application
        };
		*/

		Self {
			repo: env.repo.clone(),
			items: ItemBatch::default(),
			marked: Vec::with_capacity(2),
			selection: 0,
			highlighted_selection: None,
			commits: IndexSet::new(),
			graph_cache: RefCell::new(GitGraphCache::default()),
			highlights: None,
			scroll_state: (Instant::now(), 0_f32),
			tags: None,
			local_branches: BTreeMap::default(),
			remote_branches: BTreeMap::default(),
			current_size: Cell::new(None),
			scroll_top: Cell::new(0),
			scroll_page: RefCell::new(0),
			theme: env.theme.clone(),
			queue: env.queue.clone(),
			key_config: env.key_config.clone(),
			title: title.into(),
		}
	}

	///
	pub const fn tags(&self) -> Option<&Tags> {
		self.tags.as_ref()
	}

	///
	pub fn clear(&mut self) {
		self.items.clear();
		self.commits.clear();
	}

	///
	pub fn copy_items(&self) -> Vec<CommitId> {
		self.commits.iter().copied().collect_vec()
	}

	///
	pub fn set_tags(&mut self, tags: Tags) {
		self.tags = Some(tags);
	}

	///
	pub fn selected_entry(&self) -> Option<&LogEntry> {
		self.items.iter().nth(
			self.selection.saturating_sub(self.items.index_offset()),
		)
	}

	///
	pub fn marked_count(&self) -> usize {
		self.marked.len()
	}

	///
	pub fn clear_marked(&mut self) {
		self.marked.clear();
	}

	///
	pub fn marked_commits(&self) -> Vec<CommitId> {
		let (_, commits): (Vec<_>, Vec<CommitId>) =
			self.marked.iter().copied().unzip();

		commits
	}

	/// Build string of marked or selected (if none are marked) commit ids
	fn concat_selected_commit_ids(&self) -> Option<String> {
		match self.marked.as_slice() {
			[] => self
				.items
				.iter()
				.nth(
					self.selection
						.saturating_sub(self.items.index_offset()),
				)
				.map(|e| e.id.to_string()),
			marked => Some(
				marked
					.iter()
					.map(|(_idx, commit)| commit.to_string())
					.join(" "),
			),
		}
	}

	/// Copy currently marked or selected (if none are marked) commit ids
	/// to clipboard
	pub fn copy_commit_hash(&self) -> Result<()> {
		if let Some(yank) = self.concat_selected_commit_ids() {
			crate::clipboard::copy_string(&yank)?;
			self.queue.push(InternalEvent::ShowInfoMsg(
				strings::copy_success(&yank),
			));
		}
		Ok(())
	}

	///
	pub fn checkout(&self) {
		if let Some(commit_hash) =
			self.selected_entry().map(|entry| entry.id)
		{
			try_or_popup!(
				self,
				"failed to checkout commit:",
				checkout_commit(&self.repo.borrow(), commit_hash)
			);
		}
	}

	///
	pub fn set_local_branches(
		&mut self,
		local_branches: Vec<BranchInfo>,
	) {
		self.local_branches.clear();

		for local_branch in local_branches {
			self.local_branches
				.entry(local_branch.top_commit)
				.or_default()
				.push(local_branch);
		}
	}

	///
	pub fn set_remote_branches(
		&mut self,
		remote_branches: Vec<BranchInfo>,
	) {
		self.remote_branches.clear();

		for remote_branch in remote_branches {
			self.remote_branches
				.entry(remote_branch.top_commit)
				.or_default()
				.push(remote_branch);
		}
	}

	///
	pub fn set_commits(&mut self, commits: IndexSet<CommitId>) {
		if commits != self.commits {
			self.items.clear();
			self.commits = commits;
			self.fetch_commits(false);
		}
	}

	///
	pub fn refresh_extend_data(&mut self, commits: Vec<CommitId>) {
		let new_commits = !commits.is_empty();
		self.commits.extend(commits);

		let selection = self.selection();
		let selection_max = self.selection_max();

		if self.needs_data(selection, selection_max) || new_commits {
			self.fetch_commits(false);
		}
	}

	///
	pub fn set_highlighting(
		&mut self,
		highlighting: Option<Rc<IndexSet<CommitId>>>,
	) {
		//note: set highlights to none if there is no highlight
		self.highlights = if highlighting
			.as_ref()
			.is_some_and(|set| set.is_empty())
		{
			None
		} else {
			highlighting
		};

		self.select_next_highlight();
		self.set_highlighted_selection_index();
		self.fetch_commits(true);
	}

	///
	pub fn select_commit(&mut self, id: CommitId) -> Result<()> {
		let index = self.commits.get_index_of(&id);

		if let Some(index) = index {
			self.selection = index;
			self.set_highlighted_selection_index();
			Ok(())
		} else {
			anyhow::bail!("Could not select commit. It might not be loaded yet or it might be on a different branch.");
		}
	}

	///
	pub fn highlighted_selection_info(&self) -> (usize, usize) {
		let amount = self
			.highlights
			.as_ref()
			.map(|highlights| highlights.len())
			.unwrap_or_default();
		(self.highlighted_selection.unwrap_or_default(), amount)
	}

	fn set_highlighted_selection_index(&mut self) {
		self.highlighted_selection =
			self.highlights.as_ref().and_then(|highlights| {
				highlights.iter().position(|entry| {
					entry == &self.commits[self.selection]
				})
			});
	}

	const fn selection(&self) -> usize {
		self.selection
	}

	/// will return view size or None before the first render
	fn current_size(&self) -> Option<(u16, u16)> {
		self.current_size.get()
	}

	/// Last valid commit index
	fn selection_max(&self) -> usize {
		self.commits.len().saturating_sub(1)
	}

	fn selected_entry_marked(&self) -> bool {
		self.selected_entry()
			.and_then(|e| self.is_marked(&e.id))
			.unwrap_or_default()
	}

	fn move_selection(&mut self, scroll: ScrollType) -> Result<bool> {
		let needs_update = if self.items.highlighting() {
			self.move_selection_highlighting(scroll)?
		} else {
			self.move_selection_normal(scroll)?
		};

		Ok(needs_update)
	}

	fn move_selection_highlighting(
		&mut self,
		scroll: ScrollType,
	) -> Result<bool> {
		let (current_index, selection_max) =
			self.highlighted_selection_info();

		let new_index = match scroll {
			ScrollType::Up => current_index.saturating_sub(1),
			ScrollType::Down => current_index.saturating_add(1),

			//TODO: support this?
			// ScrollType::Home => 0,
			// ScrollType::End => self.selection_max(),
			_ => return Ok(false),
		};

		let new_index =
			cmp::min(new_index, selection_max.saturating_sub(1));

		let index_changed = new_index != current_index;

		if !index_changed {
			return Ok(false);
		}

		let new_selected_commit =
			self.highlights.as_ref().and_then(|highlights| {
				highlights.iter().nth(new_index).copied()
			});

		if let Some(c) = new_selected_commit {
			self.select_commit(c)?;
			return Ok(true);
		}

		Ok(false)
	}

	fn move_selection_normal(
		&mut self,
		scroll: ScrollType,
	) -> Result<bool> {
		self.update_scroll_speed();

		#[allow(clippy::cast_possible_truncation)]
		let speed_int = usize::try_from(self.scroll_state.1 as i64)?.max(1);

		let page_offset = usize::from(
			*self.scroll_page.borrow()
		)
		.saturating_sub(1);

		let new_selection = match scroll {
			ScrollType::Up => {
				self.selection.saturating_sub(speed_int)
			}
			ScrollType::Down => {
				self.selection.saturating_add(speed_int)
			}
			ScrollType::PageUp => {
				self.selection.saturating_sub(page_offset)
			}
			ScrollType::PageDown => {
				self.selection.saturating_add(page_offset)
			}
			ScrollType::Home => 0,
			ScrollType::End => self.selection_max(),
		};

		let new_selection =
			cmp::min(new_selection, self.selection_max());
		let needs_update = new_selection != self.selection;

		self.selection = new_selection;

		Ok(needs_update)
	}

	// Toggle mark on selected commit
	fn mark(&mut self) {
		if let Some(e) = self.selected_entry() {
			let id = e.id;
			// Index into self.items.items
			let selected_item = self
				.selection
				.saturating_sub(self.items.index_offset());
			if self.is_marked(&id).unwrap_or_default() {
				self.marked.retain(|marked| marked.1 != id);
			} else {
				self.marked.push((selected_item, id));

				self.marked.sort_unstable_by(|first, second| {
					first.0.cmp(&second.0)
				});
			}
		}
	}

	fn update_scroll_speed(&mut self) {
		const REPEATED_SCROLL_THRESHOLD_MILLIS: u128 = 300;
		const SCROLL_SPEED_START: f32 = 0.1_f32;
		const SCROLL_SPEED_MAX: f32 = 10_f32;
		const SCROLL_SPEED_MULTIPLIER: f32 = 1.05_f32;

		let now = Instant::now();

		let since_last_scroll =
			now.duration_since(self.scroll_state.0);

		self.scroll_state.0 = now;

		let speed = if since_last_scroll.as_millis()
			< REPEATED_SCROLL_THRESHOLD_MILLIS
		{
			self.scroll_state.1 * SCROLL_SPEED_MULTIPLIER
		} else {
			SCROLL_SPEED_START
		};

		self.scroll_state.1 = speed.min(SCROLL_SPEED_MAX);
	}

	fn is_marked(&self, id: &CommitId) -> Option<bool> {
		if self.marked.is_empty() {
			None
		} else {
			let found =
				self.marked.iter().any(|entry| entry.1 == *id);
			Some(found)
		}
	}

	#[allow(clippy::too_many_arguments)]
	fn get_entry_to_add<'a>(
		&self,
		e: &'a LogEntry,
		selected: bool,
		tags: Option<String>,
		local_branches: Option<String>,
		remote_branches: Option<String>,
		theme: &Theme,
		width: usize,
		now: DateTime<Local>,
		marked: Option<bool>,
	) -> Line<'a> {
		self.get_graph_entry_to_add(
			None, // graph: Option(String),
			e, //: &'a LogEntry,
			selected, //: bool,
			tags, //: Option<String>,
			local_branches, //: Option<String>,
			remote_branches,
			theme, //: &Theme,
			width, //: usize,
			now, //: DateTime<Local>,
			marked) //: Option<bool>,
	}

	#[allow(clippy::too_many_arguments)]
	fn get_graph_entry_to_add<'a>(
		&self,
		graph: Option<String>,
		e: &'a LogEntry,
		selected: bool,
		tags: Option<String>,
		local_branches: Option<String>,
		remote_branches: Option<String>,
		theme: &Theme,
		width: usize,
		now: DateTime<Local>,
		marked: Option<bool>,
	) -> Line<'a> {
		let mut txt: Vec<Span> = Vec::with_capacity(
			ELEMENTS_PER_LINE
			+ if marked.is_some() { 2 } else { 0 }
			+ if graph.is_some() { 2 } else { 0 }
			,
		);

		let normal = !self.items.highlighting()
			|| (self.items.highlighting() && e.highlighted);

		let splitter_txt = Cow::from(symbol::EMPTY_SPACE);
		let splitter = Span::styled(
			splitter_txt,
			if normal {
				theme.text(true, selected)
			} else {
				Style::default()
			},
		);

		// marker
		if let Some(marked) = marked {
			txt.push(Span::styled(
				Cow::from(if marked {
					symbol::CHECKMARK
				} else {
					symbol::EMPTY_SPACE
				}),
				theme.log_marker(selected),
			));
			txt.push(splitter.clone());
		}

		// graph
		if let Some(graph) = graph {
			// TODO decode escape codes from git-graph
			txt.push(Span::raw(	Cow::from(graph) ));
			txt.push(splitter.clone());
		}

		let style_hash = normal
			.then(|| theme.commit_hash(selected))
			.unwrap_or_else(|| theme.commit_unhighlighted());
		let style_time = normal
			.then(|| theme.commit_time(selected))
			.unwrap_or_else(|| theme.commit_unhighlighted());
		let style_author = normal
			.then(|| theme.commit_author(selected))
			.unwrap_or_else(|| theme.commit_unhighlighted());
		let style_tags = normal
			.then(|| theme.tags(selected))
			.unwrap_or_else(|| theme.commit_unhighlighted());
		let style_branches = normal
			.then(|| theme.branch(selected, true))
			.unwrap_or_else(|| theme.commit_unhighlighted());
		let style_msg = normal
			.then(|| theme.text(true, selected))
			.unwrap_or_else(|| theme.commit_unhighlighted());

		// commit hash
		txt.push(Span::styled(Cow::from(&*e.hash_short), style_hash));

		txt.push(splitter.clone());

		// commit timestamp
		txt.push(Span::styled(
			Cow::from(e.time_to_string(now)),
			style_time,
		));

		txt.push(splitter.clone());

		let author_width =
			(width.saturating_sub(19) / 3).clamp(3, 20);
		let author = string_width_align(&e.author, author_width);

		// commit author
		txt.push(Span::styled(author, style_author));

		txt.push(splitter.clone());

		// commit tags
		if let Some(tags) = tags {
			txt.push(splitter.clone());
			txt.push(Span::styled(tags, style_tags));
		}

		if let Some(local_branches) = local_branches {
			txt.push(splitter.clone());
			txt.push(Span::styled(local_branches, style_branches));
		}
		if let Some(remote_branches) = remote_branches {
			txt.push(splitter.clone());
			txt.push(Span::styled(remote_branches, style_branches));
		}

		txt.push(splitter);

		let message_width = width.saturating_sub(
			txt.iter().map(|span| span.content.len()).sum(),
		);

		// commit msg
		txt.push(Span::styled(
			format!("{:message_width$}", &e.msg),
			style_msg,
		));

		Line::from(txt)
	}

	/// Compute text displayed in commit list at current scroll offset
	fn get_text(&self, height: usize, width: usize) -> Vec<Line> {
		if LOG_GRAPH_ENABLED {
			self.get_text_graph(height, width)
		}
		else {
			self.get_text_no_graph(height, width)
		}
	}

	// Get a GitGraph instance, either cached or fresh
	fn get_git_graph(&self) -> Ref<'_, GitGraph> {
		// Return the cached graph if it exists
		if self.graph_cache.borrow().gitgraph.is_some() {
			return Ref::map(self.graph_cache.borrow(),
				|cache| cache.gitgraph.as_ref().unwrap());
		}

		// Open the asyncgit repository as a git2 repository for GitGraph
		let repo_binding = self.repo.borrow();
		let repo_path = repo_binding.gitpath();
		let git2_repo = match Graph_Repository::open(repo_path) {
			Ok(repo) => repo,
			Err(e) => panic!("Unable to open git2 repository: {}", e), // Handle this error properly in a real application
		};

		// Create graph settings if missing
		let mut graph_cache = self.graph_cache.borrow_mut();
		graph_cache
			.settings
			.get_or_insert_with(default_graph_settings);
		let graph_settings = graph_cache.settings.as_ref()
			.expect("No graph settings present");

		// Find window of commits visible
		let skip_commits = self.scroll_top.get();
		let batch = &self.items;
		let skip_items = skip_commits - batch.index_offset();
		let mut start_rev: String = "HEAD".to_string();
		if let Some(start_log_entry) = batch.iter().skip(skip_items).next() {
			start_rev = start_log_entry.id.get_short_string();
		}

		const GRAPH_COMMIT_COUNT: usize = 200;
		let git_graph = GitGraph::new(
			git2_repo,
			graph_settings,
			Some(start_rev),
			Some(GRAPH_COMMIT_COUNT)
		).expect("Unable to initialize GitGraph");

		// Store the newly created graph in the cache
		graph_cache.gitgraph = Some(git_graph);
		graph_cache.skip = skip_commits;

		// Return a reference to the cached graph
		return Ref::map( self.graph_cache.borrow(), 
			|cache| cache.gitgraph.as_ref().unwrap() )
	}

	fn get_text_graph(&self, height: usize, width: usize) -> Vec<Line> {
		// Fetch visible part of log from repository
		// We have a number of index here
		// document line = the line number as seen by the user, assuming we
		//   are scrolling over a large list of lines. Note that one commit
		//   may take more than one line to show.
		// commit index = the number of commits from the youngest commit
		// screen row = the vertical distance from the top of the view window
		//
		// The link between commit index and document line is
		//   start_row[commit_index] = document line for commit
		// The link between screen row and document line is
		//   screen row = document line - scroll top

		let local_graph = self.get_git_graph();
		let graph_cache = self.graph_cache.borrow();
		let graph_skip: usize = graph_cache.skip;
		let graph_settings = graph_cache.settings.as_ref()
			.expect("Missing graph settings");

		// Format commits as text
		let (graph_lines, text_lines, start_row)
			= print_unicode(local_graph.deref(), &graph_settings)
			.expect("Unable to print GitGraph as unicode");

		// Convert git-graph text to ratatui text
		let top_graph_ci = self.scroll_top.get() - graph_skip;
		let top_graph_row = if top_graph_ci < start_row.len() {
			Some(start_row[top_graph_ci])
		} else { None };

		let mut graph_ci: usize = top_graph_ci;
		let mut txt: Vec<Line> = Vec::with_capacity(height);
		for i in 0..height {
			let mut spans: Vec<Span> = vec![];
			let i_row = top_graph_row.and_then(|row| Some(row + i));

 			/*
 			let SHOW_MARKER_FEATURE = false;
 			// Add a marker column if any commit is marked.
 			// Show marker if on first row of commit
 			if SHOW_MARKER_FEATURE /* any_marked */ {
 				let marked = self.is_marked(commit_id) == std::option::Option(true);
 				let is_first_commit_row = start_row[commit_index] == doc_line;
 				if marked && is_first_commit_row {
 					spans.push(MARK);
 				} else {
 				 	spans.push(SPACER);
 				}
 			}
 			*/
 			/*
 			let splitter_txt = Cow::from(symbol::EMPTY_SPACE);
 			let splitter = Span::styled(
 				splitter_txt,
 				if normal {
 					theme.text(true, selected)
 				} else {
 					Style::default()
 				},
 			);
 			*/

			fn deep_copy_span(span: &Span<'_>) -> Span<'static> {
				Span {
					content: Cow::Owned(
						//remove_ansi_escape_codes(
							span.content.to_string() 
						//)
					),
					style: span.style.clone(),
				}
			}
			fn append_ansi_text(spans: &mut Vec<Span>, ansi_text: &String) {
				let text: ratatui::text::Text = ansi_text.as_bytes().into_text().unwrap();
				log::trace!("append_ansi_text from '{}' to '{}'", 
					ansi_text, text);
				if text.lines.len() != 1 {
					panic!("Converting one line did not return one line");
				}
				for span in &text.lines[0] {
					spans.push(deep_copy_span(span));
				}
			}
			if GRAPH_COLUMN_ENABLED {
				if let Some(graph_line) = 
					i_row.and_then(|row| graph_lines.get(row)) 
				{
					append_ansi_text(&mut spans, graph_line);
				} else {
					spans.push(
						Span::raw(format!("no graph line @{}", graph_ci))
					);
				}
			}

			spans.push(Span::raw("|"));

			if let Some(text_line) = 
				i_row.and_then(|row| text_lines.get(row)) 
			{
				append_ansi_text(&mut spans, text_line);
			} else {
				spans.push(
					Span::raw(format!("no text line @{}", graph_ci))
				);
			}

			// Fill right side to reach full line length
			let unused_space = width.saturating_sub(
				spans.iter().map(|span| span.content.chars().count()).sum(),
			);	
			spans.push(Span::raw(format!("{:unused_space$}", "")));

			// Apply selection background colour on selected commit
			if graph_skip + graph_ci == self.selection {
				let selection_bg = self.theme.text(true, true).bg.unwrap();
				for span in &mut spans {
					span.style = span.style.bg(selection_bg);
				}
			}

			// Finish rendering the line
			txt.push(Line::from(spans));
			
			// Find commit for next row
			let increment_ci = 
				start_row.get(graph_ci + 1).and_then(|next_ci_row| 
					i_row.and_then(|i_row|
						Some(i_row + 1 >= *next_ci_row)
					)
				);
			match increment_ci {
				Some(true) | None => graph_ci = graph_ci + 1,
				Some(false) => (),
			}
		}

		// Set page size to number of commits rendered
		*self.scroll_page.borrow_mut() = graph_ci - top_graph_ci;

		// Return lines
		txt
	}
	/*
	fn push_marker(&self, mut& spans: Spans, doc_line: DocLine) {
		let e = 
		let marked = if any_marked {
			self.is_marked(&e.id)
		} else {
			None
		};
		// marker
		if let Some(marked) = marked {
			txt.push(Span::styled(
				Cow::from(if marked {
					symbol::CHECKMARK
				} else {
					symbol::EMPTY_SPACE
				}),
				theme.log_marker(selected),
			));
			txt.push(splitter.clone());
		}
	}
	*/
	fn get_text_no_graph(&self, height: usize, width: usize) -> Vec<Line> {
		let selection = self.relative_selection();

		let mut txt: Vec<Line> = Vec::with_capacity(height);

		let now = Local::now();

		let any_marked = !self.marked.is_empty();

		for (idx, e) in self
			.items
			.iter()
			.skip(self.scroll_top.get())
			.take(height)
			.enumerate()
		{
			let tags =
				self.tags.as_ref().and_then(|t| t.get(&e.id)).map(
					|tags| {
						tags.iter()
							.map(|t| format!("<{}>", t.name))
							.join(" ")
					},
				);

			let local_branches =
				self.local_branches.get(&e.id).map(|local_branch| {
					local_branch
						.iter()
						.map(|local_branch| {
							format!("{{{0}}}", local_branch.name)
						})
						.join(" ")
				});

			let marked = if any_marked {
				self.is_marked(&e.id)
			} else {
				None
			};

			txt.push(self.get_entry_to_add(
				e,
				idx + self.scroll_top.get() == selection,
				tags,
				local_branches,
				self.remote_branches_string(e),
				&self.theme,
				width,
				now,
				marked,
			));
		}

		txt
	}

	fn remote_branches_string(&self, e: &LogEntry) -> Option<String> {
		self.remote_branches.get(&e.id).and_then(|remote_branches| {
			let filtered_branches: Vec<_> = remote_branches
				.iter()
				.filter(|remote_branch| {
					self.local_branches.get(&e.id).map_or(
						true,
						|local_branch| {
							local_branch.iter().any(|local_branch| {
								let has_corresponding_local_branch =
									match &local_branch.details {
										BranchDetails::Local(
											details,
										) => details
											.upstream
											.as_ref()
											.is_some_and(
												|upstream| {
													upstream.reference == remote_branch.reference
												},
											),
										BranchDetails::Remote(_) => {
											false
										}
									};

								!has_corresponding_local_branch
							})
						},
					)
				})
				.map(|remote_branch| {
					format!("[{0}]", remote_branch.name)
				})
				.collect();

			if filtered_branches.is_empty() {
				None
			} else {
				Some(filtered_branches.join(" "))
			}
		})
	}

	fn relative_selection(&self) -> usize {
		self.selection.saturating_sub(self.items.index_offset())
	}

	fn select_next_highlight(&mut self) {
		if self.highlights.is_none() {
			return;
		}

		let old_selection = self.selection;

		let mut offset = 0;
		loop {
			let hit_upper_bound =
				old_selection + offset > self.selection_max();
			let hit_lower_bound = offset > old_selection;

			if !hit_upper_bound {
				self.selection = old_selection + offset;

				if self.selection_highlighted() {
					break;
				}
			}

			if !hit_lower_bound {
				self.selection = old_selection - offset;

				if self.selection_highlighted() {
					break;
				}
			}

			if hit_lower_bound && hit_upper_bound {
				self.selection = old_selection;
				break;
			}

			offset += 1;
		}
	}

	fn selection_highlighted(&self) -> bool {
		let commit = self.commits[self.selection];

		self.highlights
			.as_ref()
			.is_some_and(|highlights| highlights.contains(&commit))
	}

	/// Is idx close to reload threshold
	fn needs_data(&self, idx: usize, idx_max: usize) -> bool {
		self.items.needs_data(idx, idx_max)
	}

	// checks if first entry in items is the same commit as we expect
	fn is_list_in_sync(&self) -> bool {
		self.items
			.index_offset_raw()
			.and_then(|index| {
				self.items
					.iter()
					.next()
					.map(|item| item.id == self.commits[index])
			})
			.unwrap_or_default()
	}

	fn fetch_commits(&mut self, force: bool) {
		let want_min =
			self.selection().saturating_sub(SLICE_SIZE / 2);
		let commits = self.commits.len();

		let want_min = want_min.min(commits);

		let index_in_sync = self
			.items
			.index_offset_raw()
			.is_some_and(|index| want_min == index);

		if !index_in_sync || !self.is_list_in_sync() || force {
			let commits = sync::get_commits_info(
				&self.repo.borrow(),
				self.commits
					.iter()
					.skip(want_min)
					.take(SLICE_SIZE)
					.copied()
					.collect_vec()
					.as_slice(),
				self.current_size()
					.map_or(100u16, |size| size.0)
					.into(),
			);

			if let Ok(commits) = commits {
				self.items.set_items(
					want_min,
					commits,
					self.highlights.as_ref(),
				);
			}
		}
	}
}

impl DrawableComponent for CommitList {
	fn draw(&self, f: &mut Frame, area: Rect) -> Result<()> {
		let current_size = (
			area.width.saturating_sub(2),
			area.height.saturating_sub(2),
		);
		self.current_size.set(Some(current_size));

		let height_in_lines = current_size.1 as usize;
		let height_in_commits = (*self.scroll_page.borrow())
			.max(1);
		let selection = self.relative_selection();

		// Figure out if the selection is no longer visible
		// and the window therefore has to scroll there

		self.scroll_top.set(calc_scroll_top(
			self.scroll_top.get(),
			height_in_commits,
			selection,
		));

		let title = format!(
			"{} {}/{}",
			self.title,
			self.commits.len().saturating_sub(self.selection),
			self.commits.len(),
		);

		f.render_widget(
			Paragraph::new(
				self.get_text(
					height_in_lines,
					current_size.0 as usize,
				),
			)
			.block(
				Block::default()
					.borders(Borders::ALL)
					.title(Span::styled(
						title.as_str(),
						self.theme.title(true),
					))
					.border_style(self.theme.block(true)),
			)
			.alignment(Alignment::Left),
			area,
		);

		draw_scrollbar(
			f,
			area,
			&self.theme,
			self.commits.len(),
			self.selection,
			Orientation::Vertical,
		);

		Ok(())
	}
}

impl Component for CommitList {
	fn event(&mut self, ev: &Event) -> Result<EventState> {
		if let Event::Key(k) = ev {
			let selection_changed =
				if key_match(k, self.key_config.keys.move_up) {
					self.move_selection(ScrollType::Up)?
				} else if key_match(k, self.key_config.keys.move_down)
				{
					self.move_selection(ScrollType::Down)?
				} else if key_match(k, self.key_config.keys.shift_up)
					|| key_match(k, self.key_config.keys.home)
				{
					self.move_selection(ScrollType::Home)?
				} else if key_match(
					k,
					self.key_config.keys.shift_down,
				) || key_match(k, self.key_config.keys.end)
				{
					self.move_selection(ScrollType::End)?
				} else if key_match(k, self.key_config.keys.page_up) {
					self.move_selection(ScrollType::PageUp)?
				} else if key_match(k, self.key_config.keys.page_down)
				{
					self.move_selection(ScrollType::PageDown)?
				} else if key_match(
					k,
					self.key_config.keys.log_mark_commit,
				) {
					self.mark();
					true
				} else if key_match(
					k,
					self.key_config.keys.log_checkout_commit,
				) {
					self.checkout();
					true
				} else {
					false
				};
			return Ok(selection_changed.into());
		}

		Ok(EventState::NotConsumed)
	}

	fn commands(
		&self,
		out: &mut Vec<CommandInfo>,
		_force_all: bool,
	) -> CommandBlocking {
		out.push(CommandInfo::new(
			strings::commands::scroll(&self.key_config),
			self.selected_entry().is_some(),
			true,
		));
		out.push(CommandInfo::new(
			strings::commands::commit_list_mark(
				&self.key_config,
				self.selected_entry_marked(),
			),
			true,
			true,
		));
		CommandBlocking::PassingOn
	}
}

#[cfg(test)]
mod tests {
	use asyncgit::sync::CommitInfo;

	use super::*;

	impl Default for CommitList {
		fn default() -> Self {
			Self {
				title: String::from("").into_boxed_str(),
				selection: 0,
				highlighted_selection: Option::None,
				highlights: Option::None,
				tags: Option::None,
				items: ItemBatch::default(),
				commits: IndexSet::default(),
				marked: Vec::default(),
				scroll_top: Cell::default(),
				local_branches: BTreeMap::default(),
				remote_branches: BTreeMap::default(),
				theme: SharedTheme::default(),
				key_config: SharedKeyConfig::default(),
				scroll_state: (Instant::now(), 0.0),
				current_size: Cell::default(),
				repo: RepoPathRef::new(sync::RepoPath::Path(
					std::path::PathBuf::default(),
				)),
				queue: Queue::default(),
			}
		}
	}

	#[test]
	fn test_string_width_align() {
		assert_eq!(string_width_align("123", 3), "123");
		assert_eq!(string_width_align("123", 2), "..");
		assert_eq!(string_width_align("123", 3), "123");
		assert_eq!(string_width_align("12345", 6), "12345 ");
		assert_eq!(string_width_align("1234556", 4), "12..");
	}

	#[test]
	fn test_string_width_align_unicode() {
		assert_eq!(string_width_align("äste", 3), "ä..");
		assert_eq!(
			string_width_align("wüsten äste", 10),
			"wüsten ä.."
		);
		assert_eq!(
			string_width_align("Jon Grythe Stødle", 19),
			"Jon Grythe Stødle  "
		);
	}

	/// Build a commit list with a few commits loaded
	fn build_commit_list_with_some_commits() -> CommitList {
		let mut items = ItemBatch::default();
		let basic_commit_info = CommitInfo {
			message: String::default(),
			time: 0,
			author: String::default(),
			id: CommitId::default(),
		};
		// This just creates a sequence of fake ordered ids
		// 0000000000000000000000000000000000000000
		// 0000000000000000000000000000000000000001
		// 0000000000000000000000000000000000000002
		// ...
		items.set_items(
			2, /* randomly choose an offset */
			(0..20)
				.map(|idx| CommitInfo {
					id: CommitId::from_str_unchecked(&format!(
						"{idx:040}",
					))
					.unwrap(),
					..basic_commit_info.clone()
				})
				.collect(),
			None,
		);
		CommitList {
			items,
			selection: 4, // Randomly select one commit
			..Default::default()
		}
	}

	/// Build a value for cl.marked based on indices into cl.items
	fn build_marked_from_indices(
		cl: &CommitList,
		marked_indices: &[usize],
	) -> Vec<(usize, CommitId)> {
		let offset = cl.items.index_offset();
		marked_indices
			.iter()
			.map(|idx| {
				(*idx, cl.items.iter().nth(*idx - offset).unwrap().id)
			})
			.collect()
	}

	#[test]
	fn test_copy_commit_list_empty() {
		assert_eq!(
			CommitList::default().concat_selected_commit_ids(),
			None
		);
	}

	#[test]
	fn test_copy_commit_none_marked() {
		let cl = CommitList {
			selection: 4,
			..build_commit_list_with_some_commits()
		};
		// ids from build_commit_list_with_some_commits() are
		// offset by two, so we expect commit id 2 for
		// selection = 4
		assert_eq!(
			cl.concat_selected_commit_ids(),
			Some(String::from(
				"0000000000000000000000000000000000000002"
			))
		);
	}

	#[test]
	fn test_copy_commit_one_marked() {
		let cl = build_commit_list_with_some_commits();
		let cl = CommitList {
			marked: build_marked_from_indices(&cl, &[3]),
			..cl
		};
		assert_eq!(
			cl.concat_selected_commit_ids(),
			Some(String::from(
				"0000000000000000000000000000000000000001",
			))
		);
	}

	#[test]
	fn test_copy_commit_range_marked() {
		let cl = build_commit_list_with_some_commits();
		let cl = CommitList {
			marked: build_marked_from_indices(&cl, &[4, 5, 6, 7]),
			..cl
		};
		assert_eq!(
			cl.concat_selected_commit_ids(),
			Some(String::from(concat!(
				"0000000000000000000000000000000000000002 ",
				"0000000000000000000000000000000000000003 ",
				"0000000000000000000000000000000000000004 ",
				"0000000000000000000000000000000000000005"
			)))
		);
	}

	#[test]
	fn test_copy_commit_random_marked() {
		let cl = build_commit_list_with_some_commits();
		let cl = CommitList {
			marked: build_marked_from_indices(&cl, &[4, 7]),
			..cl
		};
		assert_eq!(
			cl.concat_selected_commit_ids(),
			Some(String::from(concat!(
				"0000000000000000000000000000000000000002 ",
				"0000000000000000000000000000000000000005"
			)))
		);
	}
}
