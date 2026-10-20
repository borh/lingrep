# Token lines whose FORM/LEMMA contain a literal tab split into 13 columns
# (one extra tab each in FORM, LEMMA and the MISC Reading= value). Rewrite the
# tab as a space (same character length, so offsets are preserved) and
# re-join the split MISC pieces.
BEGIN { FS = "\t"; OFS = "\t" }
/^#/ || /^$/ || NF == 10 { print; next }
NF == 13 {
  print $1, $2 " " $3, $4 " " $5, $6, $7, $8, $9, $10, $11, $12 " " $13
  fixed++
  next
}
{ print; unfixed++; print "UNFIXED " NR ": " $0 > "/dev/stderr" }
END { print "fixed=" fixed+0 " unfixed=" unfixed+0 > "/dev/stderr" }
