# Title

Visualization of branch structure

Author: [Peer Sommerlund](mailto:peer.sommerlund@gmail.com)

**Summary:**
Parent-child relations between commits form a graph that is important
for many operations like push, rebase, etc. Gitui can list commits, but
does not show how they are related. This design shows how to integrate
the crate git-graph so the log tab shows the branch graph.

## State of the Feature as of `gitui 0.26.3`

Gitui shows all commits in a repository without regard for their parent
commit relations. This may cause commits on multiple branches to
be shown interleaved in the log tab.

## Prior work

`git log --graph` is well known, less known is git-graph which has a
cleaner visual approach.

## Goals and non-goals

The project aims to implement a graphical representation of commits.
As as secondary goal, it adds the branch sorting feature of git-graph.

It does not implement a language for filtering commits dependent on
their branch.

## Overview

The project adds the cargo git-graph and provides an alternative view
for the CommitList component, which is used in the revlog tab.

### Detailed Design

* A lazy cache of rendered text
    git-graph is used to render lines surrounding the cursor/selection
    on screen. Doing this for a large repository may use several GB of
    memory and be very slow. To get fast response and low memory usage
    only a fixed number of commits is rendered. As the user moves around
    a new set will be rendered. For very fast scrolling, the old
    rendering will be used, and replaced async when git-graph
    catches up.

* A new system for tracking location
    In order to handle multi-line commits larger than the screen, we
    track single lines. This is done with a new `struct DocLine` which
    points at a commit, and optionally a line in the commit rendering.

    TODO: Consider if this should be replaced by the old system.
    I need to show single lines, but what benefit does the user get
    from being able to address single lines? It will make sense if a commit
    has more lines than can be shown, as a way to control scrolling.

* git-graph patches
    The changes to make git-graph work inside gitui will be contributed
    upstream to git-graph. When that happens, the local copy should be
    removed. Another option is to defer merge of this PR until git-graph
    PR is merged.

* asyncgit patches
    There are a few features in asyncgit which were private. These have
    been published.

## Alternatives considered

- Show output from `git log --graph`. This would make it easy for users
to understand what is going on and how to change the format, an any
future features of git would be automatically included. The downside is
dependency on the git binary, and a slower UI. It might be difficult and
brittle to parse the output.

- Implement branch visualization from scratch. Upside is full control
over memory and UI responsiveness. Downside is a larger effort needed.

## Issues addressed (optional)

- [#81]()


## Future Possibilities

    The section for things which could be added to it or deemed out of scope during
    the discussion.
