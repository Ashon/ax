package usage

import (
	"bufio"
	"errors"
	"io"
	"os"
	"path/filepath"
	"sort"
)

// ErrNoTranscript signals that no matching transcript file was found
// for the workspace's cwd.
var ErrNoTranscript = errors.New("no transcript")

// Tailer owns the incremental read state for one workspace's transcript.
// It finds the best-matching transcript file for a cwd, tracks the byte
// offset of the last parsed line, and feeds new bytes into an Aggregator.
// When the active transcript file changes (rotated session), the tailer
// resets its Aggregator automatically.
type Tailer struct {
	cwd    string
	agg    *Aggregator
	path   string
	offset int64
}

// NewTailer constructs a tailer bound to a workspace cwd.
func NewTailer(cwd string) *Tailer {
	return &Tailer{cwd: cwd, agg: NewAggregator()}
}

// Aggregator returns the underlying aggregator (for snapshotting).
func (t *Tailer) Aggregator() *Aggregator { return t.agg }

// Path returns the currently tracked transcript path.
func (t *Tailer) Path() string { return t.path }

// Offset returns the last parsed byte offset in the current file.
func (t *Tailer) Offset() int64 { return t.offset }

// Tick resolves the active transcript via ProjectPath(cwd) and reads any
// new bytes into the aggregator. Returns the count of usage-bearing
// records ingested during this tick.
func (t *Tailer) Tick() (int, error) {
	projectDir, err := ProjectPath(t.cwd)
	if err != nil {
		return 0, err
	}
	path, err := selectTranscript(projectDir, t.cwd)
	if err != nil {
		return 0, err
	}
	return t.tickPath(path)
}

// tickPath is exported for tests that point at a fixture path directly.
func (t *Tailer) tickPath(path string) (int, error) {
	if path != t.path {
		// Different file -> reset state. This covers both "first
		// observation" and "session rotated to new file" cases.
		t.agg.Reset()
		t.path = path
		t.offset = 0
	}
	f, err := os.Open(path)
	if err != nil {
		return 0, err
	}
	defer f.Close()
	if _, err := f.Seek(t.offset, io.SeekStart); err != nil {
		return 0, err
	}
	reader := bufio.NewReader(f)
	ingested := 0
	for {
		line, err := reader.ReadBytes('\n')
		if len(line) > 0 {
			if line[len(line)-1] != '\n' {
				// Partial line at EOF: don't advance offset past it;
				// the next tick will re-read from this position.
				break
			}
			t.offset += int64(len(line))
			// Strip trailing newline.
			line = line[:len(line)-1]
			if len(line) == 0 {
				continue
			}
			if t.agg.IngestLine(line) {
				ingested++
			}
		}
		if err == io.EOF {
			break
		}
		if err != nil {
			return ingested, err
		}
	}
	return ingested, nil
}

// selectTranscript picks the best transcript for a cwd inside a Claude
// Code project dir. Rule: among *.jsonl entries, pick the one with the
// newest mtime whose first ~20 meaningful records carry a matching cwd.
// Returns ErrNoTranscript when the directory is missing or no file
// matches.
func selectTranscript(projectDir, cwd string) (string, error) {
	entries, err := os.ReadDir(projectDir)
	if err != nil {
		if os.IsNotExist(err) {
			return "", ErrNoTranscript
		}
		return "", err
	}
	type cand struct {
		path  string
		mtime int64
	}
	cands := make([]cand, 0, len(entries))
	for _, e := range entries {
		if e.IsDir() || filepath.Ext(e.Name()) != ".jsonl" {
			continue
		}
		info, err := e.Info()
		if err != nil {
			continue
		}
		cands = append(cands, cand{
			path:  filepath.Join(projectDir, e.Name()),
			mtime: info.ModTime().UnixNano(),
		})
	}
	sort.Slice(cands, func(i, j int) bool { return cands[i].mtime > cands[j].mtime })
	for _, c := range cands {
		if transcriptMatchesCwd(c.path, cwd) {
			return c.path, nil
		}
	}
	return "", ErrNoTranscript
}

// transcriptMatchesCwd checks the first ~20 records of a transcript file
// and returns true if any of them report a matching cwd. This guards
// against hash collisions between workspaces that share an encoded
// project dir name.
func transcriptMatchesCwd(path, cwd string) bool {
	f, err := os.Open(path)
	if err != nil {
		return false
	}
	defer f.Close()
	scanner := bufio.NewScanner(f)
	scanner.Buffer(make([]byte, 1024*1024), 4*1024*1024)
	seen := 0
	for scanner.Scan() && seen < 20 {
		seen++
		rec, err := parseLine(scanner.Bytes())
		if err != nil {
			continue
		}
		if rec.Cwd == cwd {
			return true
		}
	}
	return false
}
