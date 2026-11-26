// ROOT opens a file and reads every key: each object, the first entry of
// each tree, the first entry of each RNTuple. Exits 1 on any failure.
//
//   root -b -q -l 'tools/root_smoke.C("file.root")'

#include <ROOT/RNTupleReader.hxx>
#include <TFile.h>
#include <TKey.h>
#include <TROOT.h>
#include <TSystem.h>
#include <TTree.h>

#include <cstdio>
#include <string>

void root_smoke(const char *path)
{
   TFile *f = TFile::Open(path);
   if (!f || f->IsZombie()) {
      printf("ROOT cannot open %s\n", path);
      gSystem->Exit(1);
   }
   int n = 0;
   for (auto k : *f->GetListOfKeys()) {
      auto key = static_cast<TKey *>(k);
      if (std::string(key->GetClassName()) == "ROOT::RNTuple") {
         auto reader = ROOT::RNTupleReader::Open(key->GetName(), path);
         if (reader->GetNEntries() > 0)
            reader->LoadEntry(0);
      } else {
         TObject *obj = key->ReadObj();
         if (!obj) {
            printf("ROOT cannot read %s in %s\n", key->GetName(), path);
            gSystem->Exit(1);
         }
         if (auto t = dynamic_cast<TTree *>(obj))
            t->GetEntry(0);
      }
      ++n;
   }
   printf("%-50s %d keys read by ROOT %s\n", path, n, gROOT->GetVersion());
}
