// Writes, with ROOT itself, a flat TTree with one branch of every leaf type
// the writer supports: bool, (unsigned) 8/16/32/64-bit integers, float,
// double, a fixed array, a jagged array with its counter, a string. Its
// class descriptions (TTree, TBranch, TLeaf*, TIOFeatures) are captured
// into rhizoid's built-in set; the file is also a fixture.
//
//   root -b -q -l 'tests/cross/root_tree.C("root_tree.root")'
//
// With a second argument, dumps a file's tree "t" (or any tree name)
// instead: its entry count, clusters (as ROOT's cluster iterator walks
// them) and branches, then every entry, branch by branch.
//
//   root -b -q -l 'tests/cross/root_tree.C("file.root", "t")'

#include <TBranch.h>
#include <TFile.h>
#include <TLeaf.h>
#include <TSystem.h>
#include <TTree.h>

#include <cstdio>
#include <string>

static void dump(const char *path, const char *name)
{
   TFile *f = TFile::Open(path);
   if (!f || f->IsZombie()) {
      printf("FILE error cannot-open\n");
      gSystem->Exit(1);
   }
   printf("FILE recovered %d\n", f->TestBit(TFile::kRecovered) ? 1 : 0);
   auto t = f->Get<TTree>(name);
   if (!t) {
      printf("FILE error no-tree\n");
      gSystem->Exit(1);
   }
   printf("TREE entries %lld\n", t->GetEntries());
   auto clusters = t->GetClusterIterator(0);
   for (Long64_t start; (start = clusters()) < t->GetEntries();)
      printf("CLUSTER %lld %lld\n", start, clusters.GetNextEntry());
   for (auto b : *t->GetListOfBranches()) {
      auto br = static_cast<TBranch *>(b);
      auto leaf = static_cast<TLeaf *>(br->GetListOfLeaves()->At(0));
      printf("BRANCH %s %s %s\n", br->GetName(), leaf->ClassName(), br->GetTitle());
   }
   for (Long64_t i = 0; i < t->GetEntries(); ++i) {
      t->GetEntry(i);
      for (auto b : *t->GetListOfBranches()) {
         auto br = static_cast<TBranch *>(b);
         auto leaf = static_cast<TLeaf *>(br->GetListOfLeaves()->At(0));
         printf("%lld %s", i, br->GetName());
         if (std::string(leaf->ClassName()) == "TLeafC") {
            printf(" %s\n", static_cast<const char *>(leaf->GetValuePointer()));
            continue;
         }
         bool is64 = std::string(leaf->ClassName()) == "TLeafL";
         for (int j = 0; j < leaf->GetLen(); ++j) {
            if (is64 && leaf->IsUnsigned())
               printf(" %llu", (ULong64_t)leaf->GetValueLong64(j));
            else if (is64)
               printf(" %lld", leaf->GetValueLong64(j));
            else
               printf(" %.17g", leaf->GetValue(j));
         }
         printf("\n");
      }
   }
}

void root_tree(const char *path, const char *dump_name = nullptr)
{
   if (dump_name) {
      dump(path, dump_name);
      return;
   }
   TFile f(path, "RECREATE");
   TTree t("t", "every leaf type");
   bool b;
   Char_t i8;
   UChar_t u8;
   Short_t i16;
   UShort_t u16;
   Int_t i32;
   UInt_t u32;
   Long64_t i64;
   ULong64_t u64;
   Float_t f32;
   Double_t f64;
   Float_t xyz[3];
   Int_t n;
   Double_t jag[10];
   char str[64];
   t.Branch("b", &b, "b/O");
   t.Branch("i8", &i8, "i8/B");
   t.Branch("u8", &u8, "u8/b");
   t.Branch("i16", &i16, "i16/S");
   t.Branch("u16", &u16, "u16/s");
   t.Branch("i32", &i32, "i32/I");
   t.Branch("u32", &u32, "u32/i");
   t.Branch("i64", &i64, "i64/L");
   t.Branch("u64", &u64, "u64/l");
   t.Branch("f32", &f32, "f32/F");
   t.Branch("f64", &f64, "f64/D");
   t.Branch("xyz", xyz, "xyz[3]/F");
   t.Branch("n", &n, "n/I");
   t.Branch("jag", jag, "jag[n]/D");
   t.Branch("str", str, "str/C");
   for (int i = 0; i < 20; ++i) {
      b = i % 3 == 0;
      i8 = -i;
      u8 = 200 + i;
      i16 = -1000 * i;
      u16 = 60000 + i;
      i32 = -100000 * i;
      u32 = 4000000000u + i;
      i64 = -10000000000LL * i;
      u64 = 18000000000000000000ULL + i;
      f32 = 0.5f * i;
      f64 = 0.25 * i;
      for (int j = 0; j < 3; ++j)
         xyz[j] = i + 0.125f * j;
      n = i % 5;
      for (int j = 0; j < n; ++j)
         jag[j] = i * 10 + j;
      snprintf(str, sizeof str, "entry %d%s", i, i % 4 == 0 ? " is a longer string" : "");
      t.Fill();
   }
   t.Write();
   f.Close();
}
