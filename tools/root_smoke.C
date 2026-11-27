// ROOT opens a file and reads every key: each object, the first entry of
// each tree, the first entry of each RNTuple. Exits 1 on any failure.
//
//   root -b -q -l 'tools/root_smoke.C("file.root")'

#include <TROOT.h>
#if ROOT_VERSION_CODE >= ROOT_VERSION(6, 34, 0)
#include <ROOT/RNTupleReader.hxx>
#endif
#include <TFile.h>
#include <TKey.h>
#include <TSystem.h>
#include <TTree.h>

#include <cstdio>
#include <string>

// ROOT 6.34 has the RNTuple classes in ROOT::Experimental; later versions
// in ROOT.
#if ROOT_VERSION_CODE < ROOT_VERSION(6, 34, 0)
// (no RNTuple reader)
#elif ROOT_VERSION_CODE < ROOT_VERSION(6, 36, 0)
namespace RNT = ROOT::Experimental;
#else
namespace RNT = ROOT;
#endif

void root_smoke(const char *path)
{
   TFile *f = TFile::Open(path);
   if (!f || f->IsZombie()) {
      printf("ROOT cannot open %s\n", path);
      gSystem->Exit(1);
   }
   int n = 0, skipped = 0;
   for (auto k : *f->GetListOfKeys()) {
      auto key = static_cast<TKey *>(k);
      std::string cls = key->GetClassName();
#if ROOT_VERSION_CODE < ROOT_VERSION(6, 32, 0)
      // TH1L, TH2L and TH3L exist from ROOT 6.32 on.
      if (cls == "TH1L" || cls == "TH2L" || cls == "TH3L") {
         ++skipped;
         continue;
      }
#endif
      if (cls == "ROOT::RNTuple") {
#if ROOT_VERSION_CODE >= ROOT_VERSION(6, 34, 0)
         auto reader = RNT::RNTupleReader::Open(key->GetName(), path);
         if (reader->GetNEntries() > 0)
            reader->LoadEntry(0);
#else
         ++skipped;
         continue;
#endif
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
   printf("%-50s %d keys read by ROOT %s", path, n, gROOT->GetVersion());
   if (skipped)
      printf(" (%d of classes this ROOT lacks, skipped)", skipped);
   printf("\n");
}
