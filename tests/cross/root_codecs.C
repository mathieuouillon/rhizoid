// Writes, with ROOT itself, one object and one tree per compression
// setting into one file: the fixture tests/data/root_codecs.root, which
// tests/codecs.rs reads. Each key and each tree's baskets are compressed
// with the setting current when they were made.
//
// Run by tools/root-fixture.sh.

#include <TFile.h>
#include <TNamed.h>
#include <TString.h>
#include <TTree.h>

void root_codecs(const char *path)
{
   auto f = TFile::Open(path, "RECREATE");
   const struct {
      const char *name;
      int setting;
   } codecs[] = {{"zlib", 101}, {"zlib9", 109}, {"lzma", 207}, {"old", 306},
                 {"lz4", 404},  {"zstd", 505},  {"none", 0}};
   TString title;
   for (int i = 0; i < 1000; ++i)
      title += TString::Format("%d,", (i * 7) % 101);
   for (auto c : codecs) {
      f->SetCompressionSettings(c.setting);
      TNamed n(c.name, title.Data());
      n.Write();
      TTree t(TString::Format("t_%s", c.name), c.name);
      double x;
      int k;
      t.Branch("x", &x);
      t.Branch("k", &k);
      for (int i = 0; i < 2000; ++i) {
         x = i * 0.25;
         k = i % 13;
         t.Fill();
      }
      t.Write();
   }
   f->Close();
}
