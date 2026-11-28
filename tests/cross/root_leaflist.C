// Writes, with ROOT itself, a tree of leaf-list branches (one buffer per
// entry, the leaves back to back): fixed-size numbers and an array
// (`fixed`), an array counted by a leaf of the same list (`counted`), and a
// C string among numbers (`text`). The fixture of `tests/trees.rs`; with a
// second argument, prints every entry as ROOT reads it.
//
//   root -b -q -l 'tests/cross/root_leaflist.C("root_leaflist.root")'
//   root -b -q -l 'tests/cross/root_leaflist.C("root_leaflist.root", 1)'

#include <TFile.h>
#include <TTree.h>

#include <cstdio>
#include <cstring>

// Leaves are laid out back to back: no padding between members.
struct Fixed {
   Double_t e;
   Int_t run;
   Float_t p[3];
};

struct Counted {
   Int_t n;
   Float_t x[8];
};

struct Text {
   Int_t id;
   Char_t name[16];
};

static void read_back(const char *path)
{
   TFile f(path);
   auto *t = f.Get<TTree>("t");
   Fixed a{};
   Counted b{};
   Text c{};
   t->SetBranchAddress("fixed", &a);
   t->SetBranchAddress("counted", &b);
   t->SetBranchAddress("text", &c);
   for (Long64_t i = 0; i < t->GetEntries(); ++i) {
      t->GetEntry(i);
      printf("ENTRY %lld fixed %g %d %g %g %g counted %d", i, a.e, a.run, a.p[0], a.p[1], a.p[2], b.n);
      for (int k = 0; k < b.n; ++k)
         printf(" %g", b.x[k]);
      printf(" text %d %s\n", c.id, c.name);
   }
}

void root_leaflist(const char *path, int dump = 0)
{
   if (dump) {
      read_back(path);
      return;
   }
   TFile f(path, "RECREATE");
   TTree t("t", "leaf lists");
   Fixed a{};
   Counted b{};
   Text c{};
   t.Branch("fixed", &a, "e/D:run/I:p[3]/F");
   t.Branch("counted", &b, "n/I:x[n]/F");
   t.Branch("text", &c, "id/I:name/C");
   for (int i = 0; i < 6; ++i) {
      a.run = 100 + i;
      a.e = 0.5 * i;
      for (int k = 0; k < 3; ++k)
         a.p[k] = i + 0.25f * k;
      b.n = i % 4;
      for (int k = 0; k < b.n; ++k)
         b.x[k] = 10 * i + k;
      c.id = -i;
      snprintf(c.name, sizeof c.name, "entry %d", i);
      t.Fill();
   }
   t.Write();
   f.Close();
}
