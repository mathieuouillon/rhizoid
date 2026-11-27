// Writes, with ROOT itself, a tree whose branch `h` holds a whole TH1F per
// entry (split level 0): each entry is streamed as one object, its three
// TAxis members after the first referring to the class tag of the first by
// its position in the basket buffer. The fixture of `tests/trees.rs`.
//
//   root -b -q -l 'tests/cross/root_unsplit.C("root_unsplit.root")'

#include <TFile.h>
#include <TH1F.h>
#include <TTree.h>

void root_unsplit(const char *path)
{
   TFile f(path, "RECREATE");
   TTree t("t", "whole objects");
   TH1F *h = nullptr;
   t.Branch("h", &h, 32000, 0);
   for (int i = 0; i < 6; ++i) {
      TH1F entry(Form("h%d", i), Form("entry %d", i), 4, 0.0, 1.0);
      for (int k = 0; k < i; ++k)
         entry.Fill(0.125 + 0.25 * (k % 4));
      h = &entry;
      t.Fill();
   }
   h = nullptr;
   t.Write();
   f.Close();
}
